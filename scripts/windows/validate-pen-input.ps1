[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateRange(1, 65535)]
    [int]$Port,

    [Parameter(Mandatory = $true)]
    [ValidateLength(1, 160)]
    [ValidatePattern('^[^\x00-\x1f\x7f]+$')]
    [string]$ExpectedDeviceName,

    [Parameter(Mandatory = $true)]
    [ValidateLength(1, 80)]
    [ValidatePattern('^[^\x00-\x1f\x7f]+$')]
    [string]$PublicDeviceLabel,

    [ValidateRange(10, 900)]
    [int]$TimeoutSeconds = 120
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'pen-validation-lib.psm1') -Force

$appDataDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
if ([string]::IsNullOrWhiteSpace($appDataDirectory)) {
    throw 'could not resolve the canonical Windows roaming application data directory'
}
$configDirectory = Join-Path $appDataDirectory 'StreamPainter\config'

function Receive-PenValidationText {
    param(
        [Parameter(Mandatory = $true)][System.Net.WebSockets.ClientWebSocket]$Client,
        [Parameter(Mandatory = $true)][int]$RemainingMilliseconds
    )

    $buffer = New-Object byte[] 4096
    $stream = New-Object IO.MemoryStream
    $cancel = New-Object Threading.CancellationTokenSource
    try {
        $cancel.CancelAfter($RemainingMilliseconds)
        do {
            $segment = New-Object 'ArraySegment[byte]' -ArgumentList @(,$buffer)
            $result = $Client.ReceiveAsync($segment, $cancel.Token).GetAwaiter().GetResult()
            if ($result.MessageType -eq [Net.WebSockets.WebSocketMessageType]::Close) {
                throw 'local WebSocket closed before pen validation completed'
            }
            $stream.Write($buffer, 0, $result.Count)
            # A legal snapshot may contain 200,000 six-number points. 64 MiB covers the
            # protocol maximum while keeping an explicit allocation bound.
            if ($stream.Length -gt 67108864) { throw 'local WebSocket message exceeded validation limit' }
        } while (-not $result.EndOfMessage)
        if ($result.MessageType -ne [Net.WebSockets.WebSocketMessageType]::Text) {
            throw 'local WebSocket sent a non-text message'
        }
        return [Text.Encoding]::UTF8.GetString($stream.ToArray())
    }
    catch {
        # Windows PowerShell wraps some async cancellations in MethodInvocationException.
        # Only a typed OperationCanceledException cause is the deadline sentinel.
        if (Test-PenValidationCancellationError -ErrorLike $_) { return $null }
        throw
    }
    finally {
        $cancel.Dispose()
        $stream.Dispose()
    }
}

$before = Get-PenValidationConfigState -ConfigDirectory $ConfigDirectory
$environmentBefore = Get-PenValidationEnvironmentSummary `
    -Port $Port `
    -ExpectedDeviceName $ExpectedDeviceName
$verifiedDeviceBefore = Assert-PenValidationEnvironmentSummary `
    -Environment $environmentBefore `
    -ExpectedDeviceName $ExpectedDeviceName
$failure = $null
$result = $null
$state = New-PenValidationState
$client = $null
try {
    $client = New-Object Net.WebSockets.ClientWebSocket
    $origin = "http://127.0.0.1:$Port"
    $client.Options.SetRequestHeader('Origin', $origin)
    $uri = New-Object Uri "ws://127.0.0.1:$Port/ws"
    $connectCancel = New-Object Threading.CancellationTokenSource
    try {
        $connectCancel.CancelAfter(10000)
        [void]$client.ConnectAsync($uri, $connectCancel.Token).GetAwaiter().GetResult()
    }
    catch {
        if (Test-PenValidationCancellationError -ErrorLike $_) {
            throw (New-PenValidationTimeoutException -State $state)
        }
        throw
    }
    finally { $connectCancel.Dispose() }

    Write-Host "Connected. Use '$PublicDeviceLabel', select the marker, and draw at least two strokes; vary pressure and X/Y tilt in each stroke."
    $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    do {
        $remaining = [int][Math]::Ceiling(($deadline - [DateTime]::UtcNow).TotalMilliseconds)
        if ($remaining -le 0) { throw (New-PenValidationTimeoutException -State $state) }
        $completedBefore = $state.CompletedStrokes
        $message = Receive-PenValidationText $client $remaining
        if ($null -eq $message) { throw (New-PenValidationTimeoutException -State $state) }
        Add-PenValidationMessage -State $state -Message $message
        $result = Get-PenValidationResult $state
        if ($state.CompletedStrokes -gt $completedBefore) {
            Write-Host (Get-PenValidationProgressText -State $state)
        }
    } while (-not $result.Passed)
}
catch [System.TimeoutException] {
    # Emit the final sanitized state immediately as well as carrying it in the
    # exception, so a later fail-closed config check cannot hide the progress.
    Write-Host (Get-PenValidationProgressText -State $state)
    $failure = $_
}
catch { $failure = $_ }
finally {
    if ($null -ne $client) { $client.Dispose() }
}

$after = Get-PenValidationConfigState -ConfigDirectory $ConfigDirectory
if (-not (Test-PenValidationConfigUnchanged -Before $before -After $after)) {
    throw 'config.toml or config.toml.bak changed during read-only pen validation'
}
if ($null -ne $failure) { throw $failure }

$environmentAfter = Get-PenValidationEnvironmentSummary `
    -Port $Port `
    -ExpectedDeviceName $ExpectedDeviceName
$verifiedDeviceAfter = Assert-PenValidationEnvironmentSummary `
    -Environment $environmentAfter `
    -ExpectedDeviceName $ExpectedDeviceName
if ($environmentBefore.App.Sha256 -cne $environmentAfter.App.Sha256) {
    throw 'the app owning the loopback listener changed during pen validation'
}
if (-not (Test-PenValidationDeviceUnchanged -Before $verifiedDeviceBefore -After $verifiedDeviceAfter)) {
    throw 'the operator-confirmed pen device or its driver changed during pen validation'
}
$finalConfig = Get-PenValidationConfigState -ConfigDirectory $ConfigDirectory
if (-not (Test-PenValidationConfigUnchanged -Before $before -After $finalConfig)) {
    throw 'config.toml or config.toml.bak changed during read-only pen validation'
}
$summary = [pscustomobject][ordered]@{
    Result = 'passed'
    ProtocolVersion = $result.ProtocolVersion
    CompletedMarkerStrokes = $result.CompletedMarkerStrokes
    QualifiedPenStrokes = $result.QualifiedPenStrokes
    QualifiedPointCount = $result.QualifiedPointCount
    PressureRange = $result.PressureRange
    TiltXRange = $result.TiltXRange
    TiltYRange = $result.TiltYRange
    TiltXMagnitude = $result.TiltXMagnitude
    TiltYMagnitude = $result.TiltYMagnitude
    OS = $environmentAfter.OS
    OperatorConfirmedDevice = [pscustomobject][ordered]@{
        PublicModelLabel = $PublicDeviceLabel
        Class = $verifiedDeviceAfter.Class
        Status = $verifiedDeviceAfter.Status
        DriverVersion = $verifiedDeviceAfter.DriverVersion
        DriverProvider = $verifiedDeviceAfter.DriverProvider
    }
    PresentPenCandidateCount = @($environmentAfter.PresentPenDeviceCandidates).Count
    TabletServices = $environmentAfter.TabletServices
    App = $environmentAfter.App
    ConfigUnchanged = $true
}
$summary | ConvertTo-Json -Depth 5
