$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'obs-smoke-lib.psm1') -Force

function Assert-Equal {
    param(
        [Parameter(Mandatory = $true)]
        $Actual,

        [Parameter(Mandatory = $true)]
        $Expected,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    if ($Actual -ne $Expected) {
        throw "$Label`: expected '$Expected', got '$Actual'"
    }
}

function Assert-Throws {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    try {
        & $Action
    }
    catch {
        return
    }
    throw "$Label`: expected an exception"
}

function Assert-Contains {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Actual,

        [Parameter(Mandatory = $true)]
        [string]$Expected,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    if (-not $Actual.Contains($Expected)) {
        throw "$Label`: expected '$Expected' in '$Actual'"
    }
}

function Assert-ThrowsContains {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock]$Action,

        [Parameter(Mandatory = $true)]
        [string]$Expected,

        [Parameter(Mandatory = $true)]
        [string]$Label
    )

    try {
        & $Action | Out-Null
    }
    catch {
        Assert-Contains -Label $Label -Actual $_.Exception.Message -Expected $Expected
        return
    }
    throw "$Label`: expected an exception"
}

function New-WindowRecord {
    param(
        [int64]$Hwnd = 0x222,
        [uint32]$ProcessId = 4242,
        [string]$ClassName = 'stream-painter-overlay',
        [string]$Title = 'StreamPainter',
        [bool]$Visible = $true,
        [bool]$Iconic = $false,
        [bool]$RectValid = $true,
        [int]$Left = 0,
        [int]$Top = 0,
        [int]$Right = 1024,
        [int]$Bottom = 768
    )

    return [pscustomobject]@{
        ZOrder = 0
        Hwnd = $Hwnd
        ProcessId = $ProcessId
        ProcessName = 'stream-painter'
        SessionId = 1
        ThreadId = 5252
        Desktop = 'Default'
        ClassName = $ClassName
        Title = $Title
        Style = [uint32]2214592512
        ExtendedStyle = [uint32]136839336
        RectValid = $RectValid
        Left = $Left
        Top = $Top
        Right = $Right
        Bottom = $Bottom
        Visible = $Visible
        Iconic = $Iconic
    }
}

$version = '32.2.1'
$officialUri = [uri]'https://github.com/obsproject/obs-studio/releases/download/32.2.1/OBS-Studio-32.2.1-Windows-x64.zip'
$officialSha = 'db64a2934f8261f85b1410b84be011207a0afda5400d008289f1f1e211bcc7de'
Assert-ObsArchiveIdentity -Version $version -ArchiveUri $officialUri -Sha256 $officialSha
Assert-Throws -Label 'unofficial OBS host' -Action {
    Assert-ObsArchiveIdentity -Version $version -ArchiveUri ([uri]'https://example.com/OBS.zip') -Sha256 $officialSha
}
Assert-Throws -Label 'invalid OBS digest' -Action {
    Assert-ObsArchiveIdentity -Version $version -ArchiveUri $officialUri -Sha256 'abcd'
}

$authentication = Get-ObsWebSocketAuthentication `
    -Password 'supersecretpassword' `
    -Salt 'PZVbYpvAnZut2SS6JNJytDm9' `
    -Challenge 'ztTBnnuqrqaKDzRM3xcVdbYm'
Assert-Equal -Label 'obs-websocket authentication' `
    -Actual $authentication `
    -Expected 'zZgWipvwSGrw748kHN4gNpBC1IaeiiWX3Hjkrm849Sc='

$taskOutput = @(Wait-ObsSmokeTask -Task ([System.Threading.Tasks.Task]::CompletedTask))
Assert-Equal -Label 'non-generic task success output suppressed' `
    -Actual $taskOutput.Count `
    -Expected 0
$faultedTask = [System.Threading.Tasks.Task]::FromException(
    [InvalidOperationException]::new('expected smoke task failure')
)
Assert-Throws -Label 'non-generic task failure propagated' -Action {
    Wait-ObsSmokeTask -Task $faultedTask
}

$runScriptTokens = $null
$runScriptErrors = $null
$runScriptAst = [System.Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'run-obs-smoke.ps1'),
    [ref]$runScriptTokens,
    [ref]$runScriptErrors
)
Assert-Equal -Label 'run script parse errors' -Actual $runScriptErrors.Count -Expected 0
$taskWaitCalls = @(
    $runScriptAst.FindAll(
        {
            param($node)
            $node -is [System.Management.Automation.Language.CommandAst] -and
                $node.GetCommandName() -eq 'Wait-ObsSmokeTask'
        },
        $true
    )
)
Assert-Equal -Label 'non-generic WebSocket task waits' -Actual $taskWaitCalls.Count -Expected 3
$directGetResultCalls = @(
    $runScriptAst.FindAll(
        {
            param($node)
            $node -is [System.Management.Automation.Language.InvokeMemberExpressionAst] -and
                $node.Member.Value -eq 'GetResult'
        },
        $true
    )
)
Assert-Equal -Label 'direct generic WebSocket task result count' `
    -Actual $directGetResultCalls.Count `
    -Expected 1
Assert-Equal -Label 'direct task result belongs to ReceiveAsync' `
    -Actual $directGetResultCalls[0].Extent.Text.StartsWith('$Client.ReceiveAsync(') `
    -Expected $true

$runScriptText = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'run-obs-smoke.ps1'))
Assert-Equal -Label 'legacy SetCursorPos removed' `
    -Actual $runScriptText.Contains('SetCursorPos') `
    -Expected $false
Assert-Equal -Label 'legacy mouse_event removed' `
    -Actual $runScriptText.Contains('mouse_event') `
    -Expected $false
Assert-Contains -Label 'SendInput move injection' `
    -Actual $runScriptText `
    -Expected '-Flags 0xE001'
Assert-Contains -Label 'move coalescing disabled' `
    -Actual $runScriptText `
    -Expected 'MOVE_NOCOALESCE'
Assert-Contains -Label 'SendInput result count checked' `
    -Actual $runScriptText `
    -Expected '$sent -ne 1'

$nativeMethodsDefinition = $runScriptAst.FindAll(
    {
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq 'Initialize-NativeMethods'
    },
    $true
) | Select-Object -First 1
if ($null -eq $nativeMethodsDefinition) {
    throw 'Initialize-NativeMethods was not found in run-obs-smoke.ps1'
}
Invoke-Expression $nativeMethodsDefinition.Extent.Text
Initialize-NativeMethods
Assert-Equal -Label '64-bit PowerShell process' -Actual ([IntPtr]::Size) -Expected 8
Assert-Equal -Label 'x64 MOUSEINPUT size' `
    -Actual ([Runtime.InteropServices.Marshal]::SizeOf(
        [StreamPainterSmoke.NativeMethods+MouseInput]::new()
    )) `
    -Expected 32
Assert-Equal -Label 'x64 INPUT union offset' `
    -Actual ([Runtime.InteropServices.Marshal]::OffsetOf(
        [StreamPainterSmoke.NativeMethods+Input],
        'Data'
    ).ToInt32()) `
    -Expected 8
Assert-Equal -Label 'x64 INPUT size' `
    -Actual ([Runtime.InteropServices.Marshal]::SizeOf(
        [StreamPainterSmoke.NativeMethods+Input]::new()
    )) `
    -Expected 40
$sendInputMethod = [StreamPainterSmoke.NativeMethods].GetMethod('SendInput')
Assert-Equal -Label 'SendInput result supports count verification' `
    -Actual $sendInputMethod.ReturnType.FullName `
    -Expected 'System.UInt32'

$same = Get-AspectFitRect -ScreenWidth 1920 -ScreenHeight 1080
Assert-Equal -Label 'same-aspect x' -Actual $same.X -Expected 0
Assert-Equal -Label 'same-aspect y' -Actual $same.Y -Expected 0
Assert-Equal -Label 'same-aspect width' -Actual $same.Width -Expected 1920
Assert-Equal -Label 'same-aspect height' -Actual $same.Height -Expected 1080

$pillarbox = Get-AspectFitRect -ScreenWidth 2560 -ScreenHeight 1080
Assert-Equal -Label 'pillarbox x' -Actual $pillarbox.X -Expected 320
Assert-Equal -Label 'pillarbox width' -Actual $pillarbox.Width -Expected 1920

$letterbox = Get-AspectFitRect -ScreenWidth 1920 -ScreenHeight 1200
Assert-Equal -Label 'letterbox y' -Actual $letterbox.Y -Expected 60
Assert-Equal -Label 'letterbox height' -Actual $letterbox.Height -Expected 1080

Assert-Equal -Label 'absolute coordinate starts at zero' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position 0 -Origin 0 -Extent 1024) `
    -Expected 0
Assert-Equal -Label 'absolute coordinate reaches 65535' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position 1023 -Origin 0 -Extent 1024) `
    -Expected 65535
Assert-Equal -Label 'absolute coordinate first smoke endpoint' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position 256 -Origin 0 -Extent 1024) `
    -Expected 16400
Assert-Equal -Label 'absolute coordinate last smoke endpoint' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position 768 -Origin 0 -Extent 1024) `
    -Expected 49199
Assert-Equal -Label 'absolute coordinate smoke y' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position 384 -Origin 0 -Extent 768) `
    -Expected 32810
Assert-Equal -Label 'absolute coordinate respects negative virtual origin' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position -1 -Origin -1920 -Extent 1920) `
    -Expected 65535
Assert-Equal -Label 'absolute coordinate clamps low' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position -100 -Origin 0 -Extent 1024) `
    -Expected 0
Assert-Equal -Label 'absolute coordinate clamps high' `
    -Actual (ConvertTo-WindowsAbsoluteMouseCoordinate -Position 2000 -Origin 0 -Extent 1024) `
    -Expected 65535
Assert-Throws -Label 'absolute coordinate rejects one-pixel extent' -Action {
    ConvertTo-WindowsAbsoluteMouseCoordinate -Position 0 -Origin 0 -Extent 1
}

$validSnapshot = @'
{
  "type": "snapshot",
  "items": [
    {
      "kind": "stroke",
      "brush": { "color": "#ff4d6d" },
      "pts": [
        [0.25, 0.50, 1.0, 0.0, 0.0, 0.0],
        [0.75, 0.50, 1.0, 0.0, 0.0, 0.0]
      ],
      "done": true
    }
  ]
}
'@ | ConvertFrom-Json
$validDiagnostics = Assert-StreamPainterSmokeSnapshot -Snapshot $validSnapshot
Assert-Equal -Label 'semantic snapshot accepts coalesced endpoints' `
    -Actual $validDiagnostics.PointCount `
    -Expected 2
$validSummary = Format-StreamPainterSmokeStrokeDiagnostics -Diagnostics $validDiagnostics
Assert-Contains -Label 'snapshot summary kind' -Actual $validSummary -Expected 'kind="stroke"'
Assert-Contains -Label 'snapshot summary completion' -Actual $validSummary -Expected 'done=True'
Assert-Contains -Label 'snapshot summary color' -Actual $validSummary -Expected 'color="#ff4d6d"'
Assert-Contains -Label 'snapshot summary point count' -Actual $validSummary -Expected 'points=2'
Assert-Contains -Label 'snapshot summary endpoints' `
    -Actual $validSummary `
    -Expected 'first=(0.25,0.5) last=(0.75,0.5)'

$onePointSnapshot = @'
{
  "type": "snapshot",
  "items": [
    {
      "kind": "stroke",
      "brush": { "color": "#ff4d6d" },
      "pts": [[0.25, 0.50, 1.0, 0.0, 0.0, 0.0]],
      "done": true
    }
  ]
}
'@ | ConvertFrom-Json
Assert-ThrowsContains -Label 'one-point stroke diagnostic' `
    -Expected 'points=1 first=(0.25,0.5) last=(0.25,0.5)' `
    -Action { Assert-StreamPainterSmokeSnapshot -Snapshot $onePointSnapshot }

$shortSnapshot = @'
{
  "type": "snapshot",
  "items": [
    {
      "kind": "stroke",
      "brush": { "color": "#ff4d6d" },
      "pts": [
        [0.25, 0.50], [0.27, 0.50], [0.29, 0.50], [0.31, 0.50], [0.33, 0.50],
        [0.35, 0.50], [0.37, 0.50], [0.39, 0.50], [0.41, 0.50], [0.43, 0.50]
      ],
      "done": true
    }
  ]
}
'@ | ConvertFrom-Json
Assert-ThrowsContains -Label 'many-point short stroke rejected' `
    -Expected 'points=10 first=(0.25,0.5) last=(0.43,0.5)' `
    -Action { Assert-StreamPainterSmokeSnapshot -Snapshot $shortSnapshot }

$wrongMetadataSnapshot = @'
{
  "type": "snapshot",
  "items": [
    {
      "kind": "stroke",
      "brush": { "color": "#000000" },
      "pts": [[0.25, 0.50], [0.75, 0.50]],
      "done": false
    }
  ]
}
'@ | ConvertFrom-Json
Assert-ThrowsContains -Label 'stroke metadata diagnostic' `
    -Expected 'kind="stroke" done=False color="#000000" points=2' `
    -Action { Assert-StreamPainterSmokeSnapshot -Snapshot $wrongMetadataSnapshot }

$monitor = [pscustomobject]@{
    monitorPositionX = 0
    monitorPositionY = 0
    monitorWidth = 1024
    monitorHeight = 768
}
$wrongOwner = New-WindowRecord -Hwnd 0x111 -ProcessId 9999
$ownedOverlay = New-WindowRecord
$selected = Select-StreamPainterOverlayWindowRecord `
    -Windows @($wrongOwner, $ownedOverlay) `
    -ProcessId 4242 `
    -Monitor $monitor
Assert-Equal -Label 'PID-owned overlay selected' -Actual $selected.Hwnd -Expected 0x222

$invalidWindows = @(
    (New-WindowRecord -Visible $false),
    (New-WindowRecord -Iconic $true),
    (New-WindowRecord -ClassName 'not-stream-painter'),
    (New-WindowRecord -Title 'Not StreamPainter'),
    (New-WindowRecord -RectValid $false),
    (New-WindowRecord -Left 10 -Right 1034)
)
foreach ($invalidWindow in $invalidWindows) {
    $invalidSelection = Select-StreamPainterOverlayWindowRecord `
        -Windows @($invalidWindow) `
        -ProcessId 4242 `
        -Monitor $monitor
    if ($null -ne $invalidSelection) {
        throw "invalid overlay record was selected: $($invalidWindow | ConvertTo-Json -Compress)"
    }
}
Assert-Throws -Label 'ambiguous PID-owned overlays' -Action {
    Select-StreamPainterOverlayWindowRecord `
        -Windows @($ownedOverlay, (New-WindowRecord -Hwnd 0x333)) `
        -ProcessId 4242 `
        -Monitor $monitor
}

$diagnosticRecord = New-WindowRecord -Title "Stream`tPainter`nOverlay"
$windowDiagnostics = Format-TopLevelWindowDiagnostics -Windows @($diagnosticRecord)
Assert-Contains -Label 'diagnostic count' -Actual $windowDiagnostics -Expected 'top_level_window_count=1'
Assert-Contains -Label 'diagnostic PID/session' -Actual $windowDiagnostics -Expected 'pid=4242 session=1'
Assert-Contains -Label 'diagnostic class' -Actual $windowDiagnostics -Expected 'class="stream-painter-overlay"'
Assert-Contains -Label 'diagnostic escaped title' -Actual $windowDiagnostics -Expected 'title="Stream\tPainter\nOverlay"'
Assert-Contains -Label 'diagnostic styles' -Actual $windowDiagnostics -Expected 'style=0x84000000 exstyle=0x082800A8'
Assert-Contains -Label 'diagnostic rect/visibility' -Actual $windowDiagnostics -Expected 'rect=(0,0)-(1024,768) visible=True iconic=False'

Assert-Equal -Label 'smoke color accepted' `
    -Actual (Test-StreamPainterSmokePixel -Red 255 -Green 77 -Blue 109 -Alpha 255) `
    -Expected $true
Assert-Equal -Label 'black rejected' `
    -Actual (Test-StreamPainterSmokePixel -Red 0 -Green 0 -Blue 0 -Alpha 255) `
    -Expected $false
Assert-Equal -Label 'transparent smoke color rejected' `
    -Actual (Test-StreamPainterSmokePixel -Red 255 -Green 77 -Blue 109 -Alpha 0) `
    -Expected $false

Add-Type -AssemblyName System.Drawing
$testDirectory = Join-Path ([System.IO.Path]::GetTempPath()) "stream-painter-smoke-lib-$PID-$([Guid]::NewGuid().ToString('N'))"
New-Item -ItemType Directory -Path $testDirectory -Force | Out-Null
$validImage = Join-Path $testDirectory 'valid.png'
$emptyImage = Join-Path $testDirectory 'empty.png'
try {
    $bitmap = [System.Drawing.Bitmap]::new(640, 360)
    try {
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        try {
            $graphics.Clear([System.Drawing.Color]::Transparent)
            $pen = [System.Drawing.Pen]::new([System.Drawing.Color]::FromArgb(255, 255, 77, 109), 13)
            try {
                $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
                $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
                $graphics.DrawLine($pen, 160, 180, 480, 180)
            }
            finally {
                $pen.Dispose()
            }
        }
        finally {
            $graphics.Dispose()
        }
        $bitmap.Save($validImage, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $bitmap.Dispose()
    }

    $statistics = Assert-StreamPainterSmokeImage -Path $validImage
    if ($statistics.MatchingPixels -lt 200) {
        throw 'valid image should contain matching pixels'
    }

    $bitmap = [System.Drawing.Bitmap]::new(640, 360)
    try {
        $bitmap.Save($emptyImage, [System.Drawing.Imaging.ImageFormat]::Png)
    }
    finally {
        $bitmap.Dispose()
    }
    Assert-Throws -Label 'empty screenshot' -Action {
        Assert-StreamPainterSmokeImage -Path $emptyImage
    }
}
finally {
    Remove-Item -LiteralPath $testDirectory -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host 'OBS smoke helper tests passed'
