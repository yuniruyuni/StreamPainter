[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$StreamPainterExecutable,

    [Parameter(Mandatory = $true)]
    [string]$ObsVersion,

    [Parameter(Mandatory = $true)]
    [uri]$ObsArchiveUri,

    [Parameter(Mandatory = $true)]
    [string]$ObsArchiveSha256,

    [Parameter(Mandatory = $true)]
    [string]$WorkDirectory,

    [Parameter(Mandatory = $true)]
    [string]$ArtifactDirectory,

    [ValidateRange(120, 1800)]
    [int]$OverallTimeoutSeconds = 600
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'obs-smoke-lib.psm1') -Force

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'The real OBS smoke test requires Windows'
}
if ($env:GITHUB_ACTIONS -ne 'true') {
    throw 'Desktop automation is restricted to an ephemeral GitHub Actions runner; run the helper tests locally instead'
}

Assert-ObsArchiveIdentity `
    -Version $ObsVersion `
    -ArchiveUri $ObsArchiveUri `
    -Sha256 $ObsArchiveSha256

$streamPainterPath = (Resolve-Path -LiteralPath $StreamPainterExecutable -ErrorAction Stop).Path
$runRoot = Join-Path $WorkDirectory "run-$PID-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
New-Item -ItemType Directory -Path $ArtifactDirectory -Force | Out-Null
$statusPath = Join-Path $ArtifactDirectory 'smoke-status.log'
$script:OverallDeadline = [DateTimeOffset]::UtcNow.AddSeconds($OverallTimeoutSeconds)

$streamPainterProcess = $null
$obsProcess = $null
$obsConnection = $null
$obsRoot = $null
$obsPassword = $null
$diagnosticMonitor = $null
$streamPainterAppRoot = $null
$streamPainterLocalRoot = $null
$script:smokeOverlayWindowRecord = $null

function Write-SmokeStatus {
    param([Parameter(Mandatory = $true)][string]$Message)

    $line = "{0:o} {1}" -f [DateTimeOffset]::UtcNow, $Message
    Write-Host $line
    Add-Content -LiteralPath $statusPath -Value $line -Encoding UTF8
}

function Assert-WithinOverallDeadline {
    if ([DateTimeOffset]::UtcNow -ge $script:OverallDeadline) {
        throw "OBS smoke test exceeded its $OverallTimeoutSeconds-second overall timeout"
    }
}

function Invoke-WaitFor {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Description,

        [Parameter(Mandatory = $true)]
        [int]$TimeoutSeconds,

        [Parameter(Mandatory = $true)]
        [scriptblock]$Condition
    )

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastError = $null
    while ([DateTimeOffset]::UtcNow -lt $deadline -and [DateTimeOffset]::UtcNow -lt $script:OverallDeadline) {
        try {
            $result = & $Condition
            if ($result -eq $true) {
                return
            }
        }
        catch {
            $lastError = $_.Exception.Message
        }
        Start-Sleep -Milliseconds 500
    }

    $suffix = if ($null -ne $lastError) { "; last error: $lastError" } else { '' }
    throw "Timed out waiting for $Description$suffix"
}

function Write-Utf8NoBom {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$Content
    )

    $encoding = [System.Text.UTF8Encoding]::new($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Initialize-NativeMethods {
    if ($null -ne ('StreamPainterSmoke.NativeMethods' -as [type])) {
        return
    }

    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;

namespace StreamPainterSmoke
{
    public static class NativeMethods
    {
        public delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr lParam);

        [StructLayout(LayoutKind.Sequential)]
        public struct Rect
        {
            public int Left;
            public int Top;
            public int Right;
            public int Bottom;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct MouseInput
        {
            public int Dx;
            public int Dy;
            public uint MouseData;
            public uint Flags;
            public uint Time;
            public UIntPtr ExtraInfo;
        }

        [StructLayout(LayoutKind.Explicit)]
        public struct InputUnion
        {
            [FieldOffset(0)]
            public MouseInput Mouse;
        }

        [StructLayout(LayoutKind.Sequential)]
        public struct Input
        {
            public uint Type;
            public InputUnion Data;
        }

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr lParam);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsWindowVisible(IntPtr hwnd);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool IsIconic(IntPtr hwnd);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);

        [DllImport("user32.dll", SetLastError = true)]
        public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern int GetClassName(IntPtr hwnd, StringBuilder value, int maxCount);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern int GetWindowText(IntPtr hwnd, StringBuilder value, int maxCount);

        [DllImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
        public static extern IntPtr GetWindowLongPtr(IntPtr hwnd, int index);

        public static uint GetWindowStyle(IntPtr hwnd, int index)
        {
            return unchecked((uint)GetWindowLongPtr(hwnd, index).ToInt64());
        }

        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr GetProcessWindowStation();

        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr GetThreadDesktop(uint threadId);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetUserObjectInformationW", ExactSpelling = true, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetUserObjectInformationLength(
            IntPtr handle,
            int index,
            IntPtr value,
            uint length,
            out uint needed);

        [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetUserObjectInformationW", ExactSpelling = true, SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool GetUserObjectInformationName(
            IntPtr handle,
            int index,
            StringBuilder value,
            uint length,
            out uint needed);

        [DllImport("user32.dll", SetLastError = true)]
        public static extern IntPtr OpenInputDesktop(uint flags, bool inherit, uint desiredAccess);

        [DllImport("user32.dll", SetLastError = true)]
        [return: MarshalAs(UnmanagedType.Bool)]
        public static extern bool CloseDesktop(IntPtr desktop);

        [DllImport("user32.dll")]
        public static extern IntPtr GetForegroundWindow();

        [DllImport("kernel32.dll")]
        public static extern uint GetCurrentThreadId();

        [DllImport("user32.dll")]
        public static extern void keybd_event(byte virtualKey, byte scanCode, uint flags, UIntPtr extraInfo);

        [DllImport("user32.dll")]
        public static extern int GetSystemMetrics(int index);

        [DllImport("user32.dll", SetLastError = true)]
        public static extern uint SendInput(uint count, [In] Input[] inputs, int size);
    }
}
'@
}

function Get-UserObjectName {
    param([Parameter(Mandatory = $true)][IntPtr]$Handle)

    if ($Handle -eq [IntPtr]::Zero) {
        return '<unavailable>'
    }
    $needed = [uint32]0
    [void][StreamPainterSmoke.NativeMethods]::GetUserObjectInformationLength(
        $Handle,
        2,
        [IntPtr]::Zero,
        0,
        [ref]$needed
    )
    if ($needed -eq 0) {
        return "<error:$([Runtime.InteropServices.Marshal]::GetLastWin32Error())>"
    }
    $buffer = [System.Text.StringBuilder]::new([int]$needed)
    if (-not [StreamPainterSmoke.NativeMethods]::GetUserObjectInformationName(
        $Handle,
        2,
        $buffer,
        $needed,
        [ref]$needed
    )) {
        return "<error:$([Runtime.InteropServices.Marshal]::GetLastWin32Error())>"
    }
    return $buffer.ToString()
}

function Get-TopLevelWindowRecords {
    param(
        [switch]$IncludeProcessDetails,
        [uint32]$OwnerProcessId = 0
    )

    $records = [System.Collections.Generic.List[object]]::new()
    $processCache = @{}
    $desktopCache = @{}
    $resolveDetails = $IncludeProcessDetails.IsPresent
    $filterProcessId = $OwnerProcessId
    $callback = [StreamPainterSmoke.NativeMethods+EnumWindowsProc] {
        param([IntPtr]$window, [IntPtr]$unused)

        $windowProcessId = [uint32]0
        $threadId = [StreamPainterSmoke.NativeMethods]::GetWindowThreadProcessId(
            $window,
            [ref]$windowProcessId
        )
        if ($filterProcessId -ne 0 -and $windowProcessId -ne $filterProcessId) {
            return $true
        }
        $classBuffer = [System.Text.StringBuilder]::new(512)
        [void][StreamPainterSmoke.NativeMethods]::GetClassName(
            $window,
            $classBuffer,
            $classBuffer.Capacity
        )
        $titleBuffer = [System.Text.StringBuilder]::new(4096)
        [void][StreamPainterSmoke.NativeMethods]::GetWindowText(
            $window,
            $titleBuffer,
            $titleBuffer.Capacity
        )
        $rect = [StreamPainterSmoke.NativeMethods+Rect]::new()
        $rectValid = [StreamPainterSmoke.NativeMethods]::GetWindowRect($window, [ref]$rect)

        $processName = ''
        $sessionId = -1
        $desktopName = ''
        if ($resolveDetails) {
            $processKey = [string]$windowProcessId
            if (-not $processCache.ContainsKey($processKey)) {
                try {
                    $owner = [System.Diagnostics.Process]::GetProcessById([int]$windowProcessId)
                    try {
                        $processCache[$processKey] = [pscustomobject]@{
                            Name = $owner.ProcessName
                            SessionId = $owner.SessionId
                        }
                    }
                    finally {
                        $owner.Dispose()
                    }
                }
                catch {
                    $processCache[$processKey] = [pscustomobject]@{
                        Name = '<unavailable>'
                        SessionId = -1
                    }
                }
            }
            $processName = $processCache[$processKey].Name
            $sessionId = $processCache[$processKey].SessionId

            $desktopKey = [string]$threadId
            if (-not $desktopCache.ContainsKey($desktopKey)) {
                $desktopCache[$desktopKey] = Get-UserObjectName (
                    [StreamPainterSmoke.NativeMethods]::GetThreadDesktop($threadId)
                )
            }
            $desktopName = $desktopCache[$desktopKey]
        }

        $records.Add([pscustomobject]@{
            ZOrder = $records.Count
            Hwnd = $window.ToInt64()
            ProcessId = $windowProcessId
            ProcessName = $processName
            SessionId = $sessionId
            ThreadId = $threadId
            Desktop = $desktopName
            ClassName = $classBuffer.ToString()
            Title = $titleBuffer.ToString()
            Style = [StreamPainterSmoke.NativeMethods]::GetWindowStyle($window, -16)
            ExtendedStyle = [StreamPainterSmoke.NativeMethods]::GetWindowStyle($window, -20)
            RectValid = $rectValid
            Left = $rect.Left
            Top = $rect.Top
            Right = $rect.Right
            Bottom = $rect.Bottom
            Visible = [StreamPainterSmoke.NativeMethods]::IsWindowVisible($window)
            Iconic = [StreamPainterSmoke.NativeMethods]::IsIconic($window)
        })
        return $true
    }

    if (-not [StreamPainterSmoke.NativeMethods]::EnumWindows($callback, [IntPtr]::Zero)) {
        throw "EnumWindows failed with Win32 error $([Runtime.InteropServices.Marshal]::GetLastWin32Error())"
    }
    return $records.ToArray()
}

function Get-ProcessDiagnosticLine {
    param(
        [Parameter(Mandatory = $true)][string]$Label,
        [System.Diagnostics.Process]$Process
    )

    if ($null -eq $Process) {
        return "$Label=not_started"
    }
    $processId = $Process.Id
    try {
        $Process.Refresh()
        return "$Label pid=$processId session=$($Process.SessionId) exited=$($Process.HasExited) main_hwnd=0x$('{0:X}' -f $Process.MainWindowHandle.ToInt64())"
    }
    catch {
        return "$Label pid=$processId details=<unavailable:$($_.Exception.Message)>"
    }
}

function Write-WindowDiagnostics {
    param([Parameter(Mandatory = $true)][string]$Path)

    $lines = [System.Collections.Generic.List[string]]::new()
    $currentProcess = [System.Diagnostics.Process]::GetCurrentProcess()
    try {
        $lines.Add("captured_utc=$([DateTimeOffset]::UtcNow.ToString('o'))")
        $lines.Add("powershell_pid=$PID session=$($currentProcess.SessionId) version=$($PSVersionTable.PSVersion)")
    }
    finally {
        $currentProcess.Dispose()
    }
    $lines.Add("user_interactive=$([Environment]::UserInteractive) terminal_server_session=$([System.Windows.Forms.SystemInformation]::TerminalServerSession)")
    $lines.Add("environment_session_name=$env:SESSIONNAME runner_name=$env:RUNNER_NAME image_os=$env:ImageOS image_version=$env:ImageVersion")
    $lines.Add("process_window_station=$(Get-UserObjectName ([StreamPainterSmoke.NativeMethods]::GetProcessWindowStation()))")
    $lines.Add("current_thread_desktop=$(Get-UserObjectName ([StreamPainterSmoke.NativeMethods]::GetThreadDesktop([StreamPainterSmoke.NativeMethods]::GetCurrentThreadId())))")

    $inputDesktop = [StreamPainterSmoke.NativeMethods]::OpenInputDesktop(0, $false, 1)
    if ($inputDesktop -eq [IntPtr]::Zero) {
        $lines.Add("input_desktop=<error:$([Runtime.InteropServices.Marshal]::GetLastWin32Error())>")
    }
    else {
        try {
            $lines.Add("input_desktop=$(Get-UserObjectName $inputDesktop)")
        }
        finally {
            [void][StreamPainterSmoke.NativeMethods]::CloseDesktop($inputDesktop)
        }
    }

    $lines.Add((Get-ProcessDiagnosticLine -Label 'stream_painter' -Process $streamPainterProcess))
    $lines.Add((Get-ProcessDiagnosticLine -Label 'obs' -Process $obsProcess))
    $foreground = [StreamPainterSmoke.NativeMethods]::GetForegroundWindow()
    $lines.Add("foreground_hwnd=0x$('{0:X}' -f $foreground.ToInt64())")
    $lines.Add('enumeration_scope=current window station and desktop')
    $windows = @(Get-TopLevelWindowRecords -IncludeProcessDetails)
    if ($null -ne $streamPainterProcess) {
        $streamPainterId = $streamPainterProcess.Id
        $ownedCount = @($windows | Where-Object { $_.ProcessId -eq $streamPainterId }).Count
        $classCount = @(
            $windows |
                Where-Object { $_.ClassName -ceq 'stream-painter-overlay' }
        ).Count
        $ownedClassCount = @(
            $windows |
                Where-Object {
                    $_.ProcessId -eq $streamPainterId -and
                        $_.ClassName -ceq 'stream-painter-overlay'
                }
        ).Count
        $lines.Add("stream_painter_owned_top_level_count=$ownedCount overlay_class_count=$classCount owned_overlay_class_count=$ownedClassCount")
    }
    $lines.Add((Format-TopLevelWindowDiagnostics -Windows $windows))
    Write-Utf8NoBom -Path $Path -Content ($lines -join [Environment]::NewLine)
}

function Send-FunctionKeyF9 {
    [StreamPainterSmoke.NativeMethods]::keybd_event(0x78, 0, 0, [UIntPtr]::Zero)
    Start-Sleep -Milliseconds 80
    [StreamPainterSmoke.NativeMethods]::keybd_event(0x78, 0, 2, [UIntPtr]::Zero)
}

function Test-OverlayTransparent {
    param([Parameter(Mandatory = $true)][IntPtr]$Window)

    $extendedStyle = [StreamPainterSmoke.NativeMethods]::GetWindowLongPtr($Window, -20).ToInt64()
    return ($extendedStyle -band 0x20) -ne 0
}

function Send-SmokeMouseInput {
    param(
        [Parameter(Mandatory = $true)][uint32]$Flags,
        [int]$X = 0,
        [int]$Y = 0,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $mouse = [StreamPainterSmoke.NativeMethods+MouseInput]::new()
    $mouse.Dx = $X
    $mouse.Dy = $Y
    $mouse.Flags = $Flags
    $mouse.ExtraInfo = [UIntPtr]::Zero
    $union = [StreamPainterSmoke.NativeMethods+InputUnion]::new()
    $union.Mouse = $mouse
    $inputRecord = [StreamPainterSmoke.NativeMethods+Input]::new()
    $inputRecord.Type = 0
    $inputRecord.Data = $union
    $inputs = [StreamPainterSmoke.NativeMethods+Input[]]@($inputRecord)
    $inputSize = [Runtime.InteropServices.Marshal]::SizeOf($inputRecord)
    $sent = [StreamPainterSmoke.NativeMethods]::SendInput(1, $inputs, $inputSize)
    if ($sent -ne 1) {
        $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        throw "SendInput failed for $Description (sent $sent of 1, Win32 error $errorCode)"
    }
}

function Send-AbsoluteSmokeMouseMove {
    param(
        [Parameter(Mandatory = $true)][int]$X,
        [Parameter(Mandatory = $true)][int]$Y,
        [Parameter(Mandatory = $true)]$VirtualDesktop,
        [Parameter(Mandatory = $true)][string]$Description
    )

    $normalizedX = ConvertTo-WindowsAbsoluteMouseCoordinate `
        -Position $X `
        -Origin $VirtualDesktop.X `
        -Extent $VirtualDesktop.Width
    $normalizedY = ConvertTo-WindowsAbsoluteMouseCoordinate `
        -Position $Y `
        -Origin $VirtualDesktop.Y `
        -Extent $VirtualDesktop.Height
    # MOVE | MOVE_NOCOALESCE | VIRTUALDESK | ABSOLUTE. Injecting an actual
    # input packet is required for Windows to promote movement to
    # WM_POINTERUPDATE; direct cursor repositioning did not do so on the runner.
    Send-SmokeMouseInput `
        -Flags 0xE001 `
        -X $normalizedX `
        -Y $normalizedY `
        -Description $Description
}

function Send-SmokeStroke {
    param([Parameter(Mandatory = $true)]$MonitorBounds)

    $content = Get-AspectFitRect `
        -ScreenWidth $MonitorBounds.Width `
        -ScreenHeight $MonitorBounds.Height
    $startX = [int][Math]::Round($MonitorBounds.X + $content.X + ($content.Width * 0.25))
    $endX = [int][Math]::Round($MonitorBounds.X + $content.X + ($content.Width * 0.75))
    $y = [int][Math]::Round($MonitorBounds.Y + $content.Y + ($content.Height * 0.5))

    $virtualDesktop = [pscustomobject]@{
        X = [StreamPainterSmoke.NativeMethods]::GetSystemMetrics(76)
        Y = [StreamPainterSmoke.NativeMethods]::GetSystemMetrics(77)
        Width = [StreamPainterSmoke.NativeMethods]::GetSystemMetrics(78)
        Height = [StreamPainterSmoke.NativeMethods]::GetSystemMetrics(79)
    }
    if ($virtualDesktop.Width -le 1 -or $virtualDesktop.Height -le 1) {
        throw "Invalid virtual desktop dimensions $($virtualDesktop.Width)x$($virtualDesktop.Height)"
    }

    Send-AbsoluteSmokeMouseMove `
        -X $startX `
        -Y $y `
        -VirtualDesktop $virtualDesktop `
        -Description 'smoke stroke start position'
    Start-Sleep -Milliseconds 150
    Send-SmokeMouseInput -Flags 0x0002 -Description 'smoke stroke left button down'
    try {
        for ($step = 1; $step -le 48; $step++) {
            $x = [int][Math]::Round($startX + (($endX - $startX) * $step / 48.0))
            Send-AbsoluteSmokeMouseMove `
                -X $x `
                -Y $y `
                -VirtualDesktop $virtualDesktop `
                -Description "smoke stroke move step $step"
            Start-Sleep -Milliseconds 20
        }
    }
    finally {
        Send-SmokeMouseInput -Flags 0x0004 -Description 'smoke stroke left button up'
    }
}

function Send-WebSocketJson {
    param(
        [Parameter(Mandatory = $true)]
        [System.Net.WebSockets.ClientWebSocket]$Client,

        [Parameter(Mandatory = $true)]
        $Payload,

        [int]$TimeoutSeconds = 15
    )

    $json = $Payload | ConvertTo-Json -Depth 30 -Compress
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $segment = [ArraySegment[byte]]::new($bytes)
    $cancellation = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
    try {
        $sendTask = $Client.SendAsync(
            $segment,
            [System.Net.WebSockets.WebSocketMessageType]::Text,
            $true,
            $cancellation.Token
        )
        Wait-ObsSmokeTask -Task $sendTask
    }
    finally {
        $cancellation.Dispose()
    }
}

function Receive-WebSocketJson {
    param(
        [Parameter(Mandatory = $true)]
        [System.Net.WebSockets.ClientWebSocket]$Client,

        [int]$TimeoutSeconds = 15
    )

    $buffer = [byte[]]::new(16384)
    $segment = [ArraySegment[byte]]::new($buffer)
    $memory = [System.IO.MemoryStream]::new()
    try {
        do {
            $cancellation = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
            try {
                $result = $Client.ReceiveAsync($segment, $cancellation.Token).GetAwaiter().GetResult()
            }
            finally {
                $cancellation.Dispose()
            }
            if ($result.MessageType -eq [System.Net.WebSockets.WebSocketMessageType]::Close) {
                throw 'WebSocket peer closed the connection'
            }
            $memory.Write($buffer, 0, $result.Count)
            if ($memory.Length -gt 4MB) {
                throw 'WebSocket smoke response exceeded 4 MiB'
            }
        } while (-not $result.EndOfMessage)

        $text = [System.Text.Encoding]::UTF8.GetString($memory.ToArray())
        return $text | ConvertFrom-Json
    }
    finally {
        $memory.Dispose()
    }
}

function Connect-WebSocket {
    [OutputType([System.Net.WebSockets.ClientWebSocket])]
    param(
        [Parameter(Mandatory = $true)][uri]$Uri,
        [string]$Origin,
        [int]$TimeoutSeconds = 10
    )

    [System.Net.WebSockets.ClientWebSocket]$client = [System.Net.WebSockets.ClientWebSocket]::new()
    if (-not [string]::IsNullOrEmpty($Origin)) {
        $client.Options.SetRequestHeader('Origin', $Origin)
    }
    $client.Options.KeepAliveInterval = [TimeSpan]::FromSeconds(10)
    $cancellation = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds($TimeoutSeconds))
    try {
        $connectTask = $client.ConnectAsync($Uri, $cancellation.Token)
        Wait-ObsSmokeTask -Task $connectTask
        return $client
    }
    catch {
        $client.Dispose()
        throw
    }
    finally {
        $cancellation.Dispose()
    }
}

function Close-WebSocket {
    param([System.Net.WebSockets.ClientWebSocket]$Client)

    if ($null -eq $Client) {
        return
    }
    try {
        if ($Client.State -eq [System.Net.WebSockets.WebSocketState]::Open) {
            $cancellation = [System.Threading.CancellationTokenSource]::new([TimeSpan]::FromSeconds(2))
            try {
                $closeTask = $Client.CloseAsync(
                    [System.Net.WebSockets.WebSocketCloseStatus]::NormalClosure,
                    'smoke test complete',
                    $cancellation.Token
                )
                Wait-ObsSmokeTask -Task $closeTask
            }
            finally {
                $cancellation.Dispose()
            }
        }
    }
    catch {
        Write-SmokeStatus "WebSocket close warning: $($_.Exception.Message)"
    }
    finally {
        $Client.Dispose()
    }
}

function Get-StreamPainterSnapshot {
    [System.Net.WebSockets.ClientWebSocket]$client = Connect-WebSocket `
        -Uri ([uri]'ws://127.0.0.1:16873/ws') `
        -Origin 'http://127.0.0.1:16873'
    try {
        return Receive-WebSocketJson -Client $client
    }
    finally {
        Close-WebSocket -Client $client
    }
}

function Connect-ObsWebSocketOnce {
    param([Parameter(Mandatory = $true)][string]$Password)

    [System.Net.WebSockets.ClientWebSocket]$client = Connect-WebSocket `
        -Uri ([uri]'ws://127.0.0.1:4455')
    try {
        $hello = Receive-WebSocketJson -Client $client
        if ($hello.op -ne 0) {
            throw "Expected obs-websocket Hello (op 0), received op $($hello.op)"
        }
        if ($hello.d.PSObject.Properties.Name -notcontains 'authentication') {
            throw 'obs-websocket authentication is unexpectedly disabled'
        }
        $authentication = Get-ObsWebSocketAuthentication `
            -Password $Password `
            -Salt $hello.d.authentication.salt `
            -Challenge $hello.d.authentication.challenge
        Send-WebSocketJson -Client $client -Payload @{
            op = 1
            d = @{
                rpcVersion = 1
                eventSubscriptions = 0
                authentication = $authentication
            }
        }
        $identified = Receive-WebSocketJson -Client $client
        if ($identified.op -ne 2) {
            throw "Expected obs-websocket Identified (op 2), received op $($identified.op)"
        }
        return [pscustomobject]@{
            Client = $client
            NextRequestId = 1
        }
    }
    catch {
        $client.Dispose()
        throw
    }
}

function Wait-ObsWebSocket {
    param(
        [Parameter(Mandatory = $true)][string]$Password,
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [int]$TimeoutSeconds = 90
    )

    $deadline = [DateTimeOffset]::UtcNow.AddSeconds($TimeoutSeconds)
    $lastError = $null
    while ([DateTimeOffset]::UtcNow -lt $deadline -and [DateTimeOffset]::UtcNow -lt $script:OverallDeadline) {
        $Process.Refresh()
        if ($Process.HasExited) {
            throw "OBS exited before obs-websocket became ready (exit code $($Process.ExitCode))"
        }
        try {
            return Connect-ObsWebSocketOnce -Password $Password
        }
        catch {
            $lastError = $_.Exception.Message
        }
        Start-Sleep -Seconds 1
    }
    throw "Timed out connecting to authenticated obs-websocket; last error: $lastError"
}

function Invoke-ObsRequest {
    param(
        [Parameter(Mandatory = $true)]$Connection,
        [Parameter(Mandatory = $true)][string]$RequestType,
        [Parameter(Mandatory = $true)]$RequestData
    )

    Assert-WithinOverallDeadline
    $requestId = "stream-painter-smoke-$($Connection.NextRequestId)"
    $Connection.NextRequestId++
    Send-WebSocketJson -Client $Connection.Client -Payload @{
        op = 6
        d = @{
            requestType = $RequestType
            requestId = $requestId
            requestData = $RequestData
        }
    }

    while ($true) {
        $message = Receive-WebSocketJson -Client $Connection.Client
        if ($message.op -ne 7 -or $message.d.requestId -ne $requestId) {
            continue
        }
        if ($message.d.requestStatus.result -ne $true) {
            $comment = if ($message.d.requestStatus.PSObject.Properties.Name -contains 'comment') {
                $message.d.requestStatus.comment
            }
            else {
                ''
            }
            throw "OBS request $RequestType failed (code $($message.d.requestStatus.code)): $comment"
        }
        if ($message.d.PSObject.Properties.Name -contains 'responseData') {
            return $message.d.responseData
        }
        return $null
    }
}

function Get-ObsFullscreenWindowHandles {
    param(
        [Parameter(Mandatory = $true)][uint32]$ProcessId,
        [Parameter(Mandatory = $true)]$Monitor
    )

    $handles = [System.Collections.Generic.List[Int64]]::new()
    $callback = [StreamPainterSmoke.NativeMethods+EnumWindowsProc] {
        param([IntPtr]$window, [IntPtr]$unused)

        if (-not [StreamPainterSmoke.NativeMethods]::IsWindowVisible($window) -or
            [StreamPainterSmoke.NativeMethods]::IsIconic($window)) {
            return $true
        }
        $windowProcessId = [uint32]0
        [void][StreamPainterSmoke.NativeMethods]::GetWindowThreadProcessId($window, [ref]$windowProcessId)
        if ($windowProcessId -ne $ProcessId) {
            return $true
        }
        $rect = [StreamPainterSmoke.NativeMethods+Rect]::new()
        if (-not [StreamPainterSmoke.NativeMethods]::GetWindowRect($window, [ref]$rect)) {
            return $true
        }
        $tolerance = 2
        $covers = $rect.Left -le ($Monitor.monitorPositionX + $tolerance) -and
            $rect.Top -le ($Monitor.monitorPositionY + $tolerance) -and
            $rect.Right -ge ($Monitor.monitorPositionX + $Monitor.monitorWidth - $tolerance) -and
            $rect.Bottom -ge ($Monitor.monitorPositionY + $Monitor.monitorHeight - $tolerance)
        if ($covers) {
            $handles.Add($window.ToInt64())
        }
        return $true
    }
    [void][StreamPainterSmoke.NativeMethods]::EnumWindows($callback, [IntPtr]::Zero)
    return $handles.ToArray()
}

function Save-DesktopScreenshot {
    param(
        [Parameter(Mandatory = $true)]$Monitor,
        [Parameter(Mandatory = $true)][string]$Path
    )

    Add-Type -AssemblyName System.Drawing
    $bitmap = [System.Drawing.Bitmap]::new(
        [int]$Monitor.monitorWidth,
        [int]$Monitor.monitorHeight,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb
    )
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.CopyFromScreen(
                [int]$Monitor.monitorPositionX,
                [int]$Monitor.monitorPositionY,
                0,
                0,
                $bitmap.Size,
                [System.Drawing.CopyPixelOperation]::SourceCopy
            )
        }
        finally {
            $graphics.Dispose()
        }
        $bitmap.Save($Path, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $bitmap.Dispose()
    }
}

function Stop-ProcessTree {
    param(
        [System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$Label
    )

    if ($null -eq $Process) {
        return
    }
    try {
        $Process.Refresh()
        if (-not $Process.HasExited) {
            Write-SmokeStatus "Stopping $Label process tree (PID $($Process.Id))"
            $cleanupLog = Join-Path $ArtifactDirectory "$Label-cleanup.log"
            & taskkill.exe /PID $Process.Id /T /F 2>&1 | Out-File -LiteralPath $cleanupLog -Encoding utf8
        }
    }
    catch {
        Write-SmokeStatus "Failed to stop $Label process tree: $($_.Exception.Message)"
    }
}

function Copy-Diagnostics {
    if ($null -ne $obsRoot) {
        $obsLogs = Join-Path $obsRoot 'config\obs-studio\logs'
        if (Test-Path -LiteralPath $obsLogs) {
            Copy-Item -LiteralPath $obsLogs -Destination (Join-Path $ArtifactDirectory 'obs-logs') -Recurse -Force
        }
    }
    $streamPainterLogs = Join-Path $env:LOCALAPPDATA 'StreamPainter\logs'
    if (Test-Path -LiteralPath $streamPainterLogs) {
        Copy-Item -LiteralPath $streamPainterLogs -Destination (Join-Path $ArtifactDirectory 'stream-painter-logs') -Recurse -Force
    }

    if (-not [string]::IsNullOrEmpty($obsPassword)) {
        Get-ChildItem -LiteralPath $ArtifactDirectory -Recurse -File |
            Where-Object { $_.Extension -in @('.log', '.txt', '.json', '.ini') } |
            ForEach-Object {
                try {
                    $text = [System.IO.File]::ReadAllText($_.FullName)
                    if ($text.Contains($obsPassword)) {
                        [System.IO.File]::WriteAllText(
                            $_.FullName,
                            $text.Replace($obsPassword, '[REDACTED]'),
                            [System.Text.UTF8Encoding]::new($false)
                        )
                    }
                }
                catch {
                    Write-SmokeStatus "Could not redact diagnostic file $($_.FullName): $($_.Exception.Message)"
                }
            }
    }
}

Write-SmokeStatus "Starting real OBS smoke test with OBS Studio $ObsVersion"
Write-SmokeStatus "Runner image: $env:RUNNER_OS / $env:ImageOS / $env:ImageVersion"

try {
    Initialize-NativeMethods
    Add-Type -AssemblyName System.Windows.Forms

    $primaryBounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $diagnosticMonitor = [pscustomobject]@{
        monitorPositionX = $primaryBounds.X
        monitorPositionY = $primaryBounds.Y
        monitorWidth = $primaryBounds.Width
        monitorHeight = $primaryBounds.Height
    }
    Write-SmokeStatus "Primary desktop is $($primaryBounds.Width)x$($primaryBounds.Height) at ($($primaryBounds.X),$($primaryBounds.Y))"

    $candidateAppRoot = Join-Path $env:APPDATA 'StreamPainter'
    $candidateLocalRoot = Join-Path $env:LOCALAPPDATA 'StreamPainter'
    foreach ($candidateRoot in @($candidateAppRoot, $candidateLocalRoot)) {
        if (Test-Path -LiteralPath $candidateRoot) {
            throw "Runner profile is not clean: $candidateRoot already exists"
        }
    }
    # Only paths proven absent above are eligible for cleanup in finally.
    $streamPainterAppRoot = $candidateAppRoot
    $streamPainterLocalRoot = $candidateLocalRoot
    $configDirectory = Join-Path $streamPainterAppRoot 'config'
    New-Item -ItemType Directory -Path $configDirectory -Force | Out-Null
    $config = @'
local_server_port = 16873
screen = 0
canvas_aspect = "16:9"
local_echo = true
follow_projector = false
obs_control = false
obs_websocket_url = "ws://127.0.0.1:4455"
projector_view = "program"
close_projector = true

[brush]
color = "#ff4d6d"
width_n = 0.035
'@
    Write-Utf8NoBom -Path (Join-Path $configDirectory 'config.toml') -Content $config

    $streamPainterProcess = Start-Process `
        -FilePath $streamPainterPath `
        -PassThru `
        -RedirectStandardOutput (Join-Path $ArtifactDirectory 'stream-painter-stdout.txt') `
        -RedirectStandardError (Join-Path $ArtifactDirectory 'stream-painter-stderr.txt')
    Write-SmokeStatus "StreamPainter started (PID $($streamPainterProcess.Id))"

    Invoke-WaitFor -Description 'StreamPainter /health' -TimeoutSeconds 45 -Condition {
        $streamPainterProcess.Refresh()
        if ($streamPainterProcess.HasExited) {
            throw "StreamPainter exited with code $($streamPainterProcess.ExitCode)"
        }
        try {
            $response = Invoke-WebRequest `
                -Uri 'http://127.0.0.1:16873/health' `
                -TimeoutSec 2 `
                -UseBasicParsing
            return $response.StatusCode -eq 200 -and $response.Content.Trim() -eq 'ok'
        }
        catch {
            return $false
        }
    }
    Write-SmokeStatus 'StreamPainter /health returned 200 ok'

    $overlayWindow = [IntPtr]::Zero
    Invoke-WaitFor -Description 'StreamPainter overlay window' -TimeoutSeconds 30 -Condition {
        $streamPainterProcess.Refresh()
        if ($streamPainterProcess.HasExited) {
            throw "StreamPainter exited before its overlay appeared (exit code $($streamPainterProcess.ExitCode))"
        }
        $windowRecords = @(
            Get-TopLevelWindowRecords -OwnerProcessId ([uint32]$streamPainterProcess.Id)
        )
        $ownedOverlay = Select-StreamPainterOverlayWindowRecord `
            -Windows $windowRecords `
            -ProcessId ([uint32]$streamPainterProcess.Id) `
            -Monitor $diagnosticMonitor
        if ($null -ne $ownedOverlay) {
            $script:smokeOverlayWindowRecord = $ownedOverlay
            return $true
        }
        return $false
    }
    $overlayRecord = $script:smokeOverlayWindowRecord
    $overlayWindow = [IntPtr]::new([int64]$overlayRecord.Hwnd)
    Write-SmokeStatus (
        "Found PID-owned StreamPainter overlay hwnd=0x$('{0:X}' -f $overlayWindow.ToInt64()) " +
        "class='$($overlayRecord.ClassName)' title='$($overlayRecord.Title)' " +
        "rect=($($overlayRecord.Left),$($overlayRecord.Top))-($($overlayRecord.Right),$($overlayRecord.Bottom)) " +
        "visible=$($overlayRecord.Visible)"
    )
    if (-not (Test-OverlayTransparent -Window $overlayWindow)) {
        throw 'StreamPainter overlay did not start in click-through mode'
    }

    Send-FunctionKeyF9
    Invoke-WaitFor -Description 'StreamPainter draw mode' -TimeoutSeconds 10 -Condition {
        return -not (Test-OverlayTransparent -Window $overlayWindow)
    }
    Send-SmokeStroke -MonitorBounds $primaryBounds
    Start-Sleep -Seconds 1
    Send-FunctionKeyF9
    Invoke-WaitFor -Description 'StreamPainter click-through mode after drawing' -TimeoutSeconds 10 -Condition {
        return Test-OverlayTransparent -Window $overlayWindow
    }
    Write-SmokeStatus 'Injected one real F9/mouse stroke before OBS startup'

    $snapshot = Get-StreamPainterSnapshot
    $snapshotPath = Join-Path $ArtifactDirectory 'stream-painter-snapshot.json'
    Write-Utf8NoBom `
        -Path $snapshotPath `
        -Content ($snapshot | ConvertTo-Json -Depth 30)
    Write-SmokeStatus 'Saved pre-OBS StreamPainter snapshot artifact'
    $snapshotDiagnostics = Assert-StreamPainterSmokeSnapshot -Snapshot $snapshot
    Write-SmokeStatus (
        'Verified pre-OBS snapshot: ' +
        (Format-StreamPainterSmokeStrokeDiagnostics -Diagnostics $snapshotDiagnostics)
    )

    Assert-WithinOverallDeadline
    $archivePath = Join-Path $runRoot "OBS-Studio-$ObsVersion-Windows-x64.zip"
    Write-SmokeStatus "Downloading pinned OBS archive from $($ObsArchiveUri.AbsoluteUri)"
    Invoke-WebRequest `
        -Uri $ObsArchiveUri `
        -OutFile $archivePath `
        -TimeoutSec 180 `
        -UseBasicParsing
    $actualSha = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualSha -cne $ObsArchiveSha256.ToLowerInvariant()) {
        throw "OBS archive SHA-256 mismatch: expected $ObsArchiveSha256, got $actualSha"
    }
    Write-SmokeStatus "Verified OBS archive SHA-256 $actualSha"

    $extractRoot = Join-Path $runRoot 'obs'
    Expand-Archive -LiteralPath $archivePath -DestinationPath $extractRoot -Force
    $obsExecutable = Get-ChildItem -LiteralPath $extractRoot -Filter 'obs64.exe' -File -Recurse |
        Where-Object { $_.Directory.Name -eq '64bit' } |
        Select-Object -First 1
    if ($null -eq $obsExecutable) {
        throw 'obs64.exe was not found in the verified archive'
    }
    $obsRoot = $obsExecutable.Directory.Parent.Parent.FullName
    $obsConfigRoot = Join-Path $obsRoot 'config\obs-studio'
    $webSocketConfigDirectory = Join-Path $obsConfigRoot 'plugin_config\obs-websocket'
    New-Item -ItemType Directory -Path $webSocketConfigDirectory -Force | Out-Null
    $globalConfig = @'
[General]
FirstRun=false
EnableAutoUpdates=false
'@
    Write-Utf8NoBom -Path (Join-Path $obsConfigRoot 'global.ini') -Content $globalConfig
    $webSocketConfig = @{
        first_load = $false
        server_enabled = $true
        server_port = 4455
        alerts_enabled = $false
        auth_required = $true
        server_password = ''
    } | ConvertTo-Json -Compress
    Write-Utf8NoBom -Path (Join-Path $webSocketConfigDirectory 'config.json') -Content $webSocketConfig

    $obsPassword = [Guid]::NewGuid().ToString('N') + [Guid]::NewGuid().ToString('N')
    $obsArguments = @(
        '--portable',
        '--multi',
        '--disable-updater',
        '--disable-missing-files-check',
        '--only-bundled-plugins',
        '--websocket_port', '4455',
        '--websocket_password', $obsPassword,
        '--websocket_ipv4_only',
        '--verbose'
    )
    $obsProcess = Start-Process `
        -FilePath $obsExecutable.FullName `
        -ArgumentList $obsArguments `
        -WorkingDirectory $obsExecutable.Directory.FullName `
        -PassThru `
        -RedirectStandardOutput (Join-Path $ArtifactDirectory 'obs-stdout.txt') `
        -RedirectStandardError (Join-Path $ArtifactDirectory 'obs-stderr.txt')
    Write-SmokeStatus "OBS Studio started in portable mode (PID $($obsProcess.Id))"

    $obsConnection = Wait-ObsWebSocket -Password $obsPassword -Process $obsProcess
    $versionInfo = Invoke-ObsRequest -Connection $obsConnection -RequestType 'GetVersion' -RequestData @{}
    if ($versionInfo.obsVersion -notlike "$ObsVersion*") {
        throw "Running OBS version '$($versionInfo.obsVersion)' does not match pinned $ObsVersion"
    }
    Write-SmokeStatus "Authenticated to real obs-websocket $($versionInfo.obsWebSocketVersion) on OBS $($versionInfo.obsVersion)"

    $inputKinds = Invoke-ObsRequest -Connection $obsConnection -RequestType 'GetInputKindList' -RequestData @{}
    if (@($inputKinds.inputKinds) -notcontains 'browser_source') {
        throw 'Verified OBS distribution did not load its Browser Source plugin'
    }

    $sceneName = 'StreamPainter Smoke Scene'
    $inputName = 'StreamPainter Smoke Browser'
    $scenes = Invoke-ObsRequest -Connection $obsConnection -RequestType 'GetSceneList' -RequestData @{}
    if (@($scenes.scenes | ForEach-Object { $_.sceneName }) -notcontains $sceneName) {
        [void](Invoke-ObsRequest -Connection $obsConnection -RequestType 'CreateScene' -RequestData @{
            sceneName = $sceneName
        })
    }
    [void](Invoke-ObsRequest -Connection $obsConnection -RequestType 'SetCurrentProgramScene' -RequestData @{
        sceneName = $sceneName
    })
    $createdInput = Invoke-ObsRequest -Connection $obsConnection -RequestType 'CreateInput' -RequestData @{
        sceneName = $sceneName
        inputName = $inputName
        inputKind = 'browser_source'
        inputSettings = @{
            url = 'http://127.0.0.1:16873/overlay'
            width = 640
            height = 360
            fps = 60
            css = ''
            shutdown = $false
            restart_when_active = $true
            reroute_audio = $false
        }
        sceneItemEnabled = $true
    }
    Write-SmokeStatus "Created real OBS Browser Source (scene item $($createdInput.sceneItemId))"

    $sourceScreenshot = Join-Path $ArtifactDirectory 'browser-source.png'
    $sourceStatistics = $null
    $sourceDeadline = [DateTimeOffset]::UtcNow.AddSeconds(90)
    $lastSourceError = $null
    while ([DateTimeOffset]::UtcNow -lt $sourceDeadline -and [DateTimeOffset]::UtcNow -lt $script:OverallDeadline) {
        try {
            Remove-Item -LiteralPath $sourceScreenshot -Force -ErrorAction SilentlyContinue
            [void](Invoke-ObsRequest -Connection $obsConnection -RequestType 'SaveSourceScreenshot' -RequestData @{
                sourceName = $inputName
                imageFormat = 'png'
                imageFilePath = $sourceScreenshot
                imageWidth = 640
                imageHeight = 360
                imageCompressionQuality = 100
            })
            $sourceStatistics = Assert-StreamPainterSmokeImage -Path $sourceScreenshot
            break
        }
        catch {
            $lastSourceError = $_.Exception.Message
            Start-Sleep -Seconds 2
        }
    }
    if ($null -eq $sourceStatistics) {
        throw "OBS Browser Source never rendered the StreamPainter snapshot: $lastSourceError"
    }
    $sourceStatistics | ConvertTo-Json | Set-Content `
        -LiteralPath (Join-Path $ArtifactDirectory 'browser-source-statistics.json') `
        -Encoding UTF8
    Write-SmokeStatus "OBS Browser Source rendered snapshot: $($sourceStatistics.MatchingPixels) matching pixels, x=$($sourceStatistics.MinX)..$($sourceStatistics.MaxX)"

    $monitorList = Invoke-ObsRequest -Connection $obsConnection -RequestType 'GetMonitorList' -RequestData @{}
    $monitors = @($monitorList.monitors)
    if ($monitors.Count -eq 0) {
        throw 'obs-websocket GetMonitorList returned no monitors'
    }
    $projectorMonitor = $monitors[0]
    $diagnosticMonitor = $projectorMonitor
    $baselineFullscreen = @(Get-ObsFullscreenWindowHandles `
        -ProcessId ([uint32]$obsProcess.Id) `
        -Monitor $projectorMonitor)
    [void](Invoke-ObsRequest -Connection $obsConnection -RequestType 'OpenVideoMixProjector' -RequestData @{
        videoMixType = 'OBS_WEBSOCKET_VIDEO_MIX_TYPE_PROGRAM'
        monitorIndex = $projectorMonitor.monitorIndex
    })
    Invoke-WaitFor -Description 'new OBS full-screen projector window' -TimeoutSeconds 45 -Condition {
        $current = @(Get-ObsFullscreenWindowHandles `
            -ProcessId ([uint32]$obsProcess.Id) `
            -Monitor $projectorMonitor)
        return @($current | Where-Object { $baselineFullscreen -notcontains $_ }).Count -gt 0
    }
    Write-SmokeStatus "Opened and detected a real OBS Program projector on monitor $($projectorMonitor.monitorIndex)"

    try {
        Save-DesktopScreenshot `
            -Monitor $projectorMonitor `
            -Path (Join-Path $ArtifactDirectory 'projector-desktop.png')
        Write-SmokeStatus 'Captured projector desktop screenshot'
    }
    catch {
        Write-SmokeStatus "Projector screenshot was unavailable: $($_.Exception.Message)"
        Set-Content `
            -LiteralPath (Join-Path $ArtifactDirectory 'projector-screenshot-error.txt') `
            -Value $_.Exception.ToString() `
            -Encoding UTF8
    }

    Set-Content `
        -LiteralPath (Join-Path $ArtifactDirectory 'success.txt') `
        -Value "OBS $ObsVersion Browser Source, snapshot, and projector smoke test passed" `
        -Encoding UTF8
    Write-SmokeStatus 'Real OBS smoke test passed'
}
catch {
    $failure = $_
    Write-SmokeStatus "FAILURE: $($failure.Exception.Message)"
    Set-Content `
        -LiteralPath (Join-Path $ArtifactDirectory 'failure.txt') `
        -Value $failure.Exception.ToString() `
        -Encoding UTF8
    try {
        $windowDiagnosticPath = Join-Path $ArtifactDirectory 'window-diagnostics.txt'
        Write-WindowDiagnostics -Path $windowDiagnosticPath
        Write-SmokeStatus 'Captured process, desktop, and top-level window diagnostics'
    }
    catch {
        Set-Content `
            -LiteralPath (Join-Path $ArtifactDirectory 'window-diagnostics-error.txt') `
            -Value $_.Exception.ToString() `
            -Encoding UTF8
    }
    if ($null -ne $diagnosticMonitor) {
        try {
            Save-DesktopScreenshot `
                -Monitor $diagnosticMonitor `
                -Path (Join-Path $ArtifactDirectory 'failure-desktop.png')
        }
        catch {
            Set-Content `
                -LiteralPath (Join-Path $ArtifactDirectory 'failure-screenshot-error.txt') `
                -Value $_.Exception.ToString() `
                -Encoding UTF8
        }
    }
    throw $failure
}
finally {
    if ($null -ne $obsConnection) {
        Close-WebSocket -Client $obsConnection.Client
    }
    Stop-ProcessTree -Process $obsProcess -Label 'obs'
    Stop-ProcessTree -Process $streamPainterProcess -Label 'stream-painter'
    try {
        Copy-Diagnostics
    }
    catch {
        Write-SmokeStatus "Failed to collect some diagnostics: $($_.Exception.Message)"
    }
    foreach ($temporaryPath in @($runRoot, $streamPainterAppRoot, $streamPainterLocalRoot)) {
        if (-not [string]::IsNullOrEmpty($temporaryPath) -and (Test-Path -LiteralPath $temporaryPath)) {
            try {
                Remove-Item -LiteralPath $temporaryPath -Recurse -Force
                Write-SmokeStatus "Removed temporary test data: $temporaryPath"
            }
            catch {
                Write-SmokeStatus "Failed to remove temporary test data $temporaryPath`: $($_.Exception.Message)"
            }
        }
    }
}
