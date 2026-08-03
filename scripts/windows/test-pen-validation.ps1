$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Import-Module (Join-Path $PSScriptRoot 'pen-validation-lib.psm1') -Force

function Assert-True { param([bool]$Value, [string]$Label) if (-not $Value) { throw "$Label failed" } }
function Assert-False { param([bool]$Value, [string]$Label) if ($Value) { throw "$Label failed" } }
function Assert-Throws {
    param([scriptblock]$Action, [string]$Expected, [string]$Label)
    try { & $Action; throw "$Label did not throw" }
    catch {
        if (-not $_.Exception.Message.Contains($Expected)) {
            throw "$Label threw '$($_.Exception.Message)', expected '$Expected'"
        }
    }
}
function New-Brush {
    return [pscustomobject]@{ tool='marker'; color='#abcdef'; opacity=0.5; widthN=0.03;
        pressureWidth=$true; pressureMin=0.65; tiltWidth=$true; tiltMaxScale=1.75 }
}
function Add-TestStroke {
    param($State, [string]$Id, [int]$Revision, [object[]]$Points)
    Add-PenValidationMessage $State ([pscustomobject]@{type='stroke_begin';rev=$Revision;strokeId=$Id;brush=(New-Brush)})
    Add-PenValidationMessage $State ([pscustomobject]@{type='stroke_points';rev=($Revision+1);strokeId=$Id;pts=$Points})
    Add-PenValidationMessage $State ([pscustomobject]@{type='stroke_end';rev=($Revision+2);strokeId=$Id;endedAt=1})
}
function New-StartedState {
    $state = New-PenValidationState
    Add-PenValidationMessage $state ([pscustomobject]@{type='snapshot';protocolVersion=7;rev=20;items=@(
        [pscustomobject]@{kind='stroke';pts=@(@(0,0,0,0,0,0))})})
    return $state
}

$emptyProgressState = New-PenValidationState
$emptyProgress = Get-PenValidationProgressText $emptyProgressState
Assert-True ($emptyProgress -ceq ('Pen validation progress: completed=0; qualified=0/2; ' +
    'last-stroke=none; missing=completed-marker-stroke,qualified-strokes=2')) `
    'empty progress identifies missing input'
$emptyTimeout = New-PenValidationTimeoutException $emptyProgressState
Assert-True ($emptyTimeout -is [System.TimeoutException]) 'timeout helper returns TimeoutException'
Assert-True ($emptyTimeout.Message.EndsWith($emptyProgress)) 'timeout includes empty final progress'
$typedTimeoutCaught = $false
try { throw $emptyTimeout }
catch [System.TimeoutException] {
    $typedTimeoutCaught = $_.Exception.Message.EndsWith($emptyProgress)
}
Assert-True $typedTimeoutCaught 'timeout remains typed when thrown and caught'

$directCancellation = New-Object System.OperationCanceledException 'validation deadline'
$wrappedCancellation = New-Object Management.Automation.MethodInvocationException `
    -ArgumentList @('wrapped cancellation', $directCancellation)
$unrelatedCause = New-Object System.InvalidOperationException 'not a timeout'
$wrappedUnrelated = New-Object Management.Automation.MethodInvocationException `
    -ArgumentList @('wrapped non-timeout', $unrelatedCause)
Assert-True (Test-PenValidationCancellationError $directCancellation) `
    'direct cancellation is a timeout cause'
Assert-True (Test-PenValidationCancellationError $wrappedCancellation) `
    'PowerShell method invocation wrapper is a timeout cause'
Assert-False (Test-PenValidationCancellationError $wrappedUnrelated) `
    'non-timeout wrapper is not swallowed'
$wrappedCancellationRecord = $null
try { throw $wrappedCancellation }
catch { $wrappedCancellationRecord = $_ }
$wrappedUnrelatedRecord = $null
try { throw $wrappedUnrelated }
catch { $wrappedUnrelatedRecord = $_ }
Assert-True (Test-PenValidationCancellationError $wrappedCancellationRecord) `
    'PowerShell ErrorRecord preserves the wrapped timeout cause'
Assert-False (Test-PenValidationCancellationError $wrappedUnrelatedRecord) `
    'PowerShell ErrorRecord preserves a non-timeout cause'
Assert-False (Test-PenValidationCancellationError $null) 'null is not a timeout cause'

$progressState = New-StartedState
$privateStrokeId = 'private-progress-stroke-id'
Add-TestStroke $progressState $privateStrokeId 21 @(
    ,@(0.123456,0.234567,0.50,0,-0.04,-0.03),
    ,@(0.876543,0.765432,0.52,1,0.04,0.03))
$progressText = Get-PenValidationProgressText $progressState
Assert-True ($progressText -ceq ('Pen validation progress: completed=1; qualified=0/2; ' +
    'last-points=2; last-pressure-range=0.020000; last-tilt-x-range=0.080000; ' +
    'last-tilt-y-range=0.060000; last-qualified=false; ' +
    'missing=pressure-range>=0.050000,qualified-strokes=2')) `
    'failed stroke progress identifies the missing pressure range'
$lastProgressProperties = @($progressState.LastCompletedStroke.PSObject.Properties.Name) -join ','
Assert-True ($lastProgressProperties -ceq `
    'PointCount,Qualified,PressureRange,TiltXRange,TiltYRange') `
    'last stroke state contains only sanitized aggregates'
Assert-False ($progressText.Contains($privateStrokeId)) 'progress excludes stroke ID'
Assert-False ($progressText.Contains('0.123456')) 'progress excludes X coordinate'
Assert-False ($progressText.Contains('0.234567')) 'progress excludes Y coordinate'
$progressTimeout = New-PenValidationTimeoutException $progressState
Assert-True ($progressTimeout.Message.EndsWith($progressText)) `
    'timeout includes final completed stroke progress'
Assert-False ($progressTimeout.Message.Contains($privateStrokeId)) 'timeout excludes stroke ID'

$exactThresholds = New-StartedState
Add-TestStroke $exactThresholds 'exact-thresholds' 21 @(
    ,@(0,0,0.10,0,0.01,0.01),,@(0,0,0.15,1,0.03,0.03))
$exactThresholdProgress = Get-PenValidationProgressText $exactThresholds
Assert-True $exactThresholds.LastCompletedStroke.Qualified `
    'mathematically exact pressure and X/Y tilt thresholds qualify'
Assert-True ($exactThresholdProgress.Contains('last-qualified=true')) `
    'exact threshold progress agrees with qualification'
Assert-True ($exactThresholdProgress.EndsWith('missing=qualified-strokes=2')) `
    'exact threshold progress only needs another qualified stroke'

$nearPressureThreshold = New-StartedState
Add-TestStroke $nearPressureThreshold 'near-pressure-threshold' 21 @(
    ,@(0,0,0.10,0,0.01,0.01),,@(0,0,0.1499996,1,0.03,0.03))
$nearPressureProgress = Get-PenValidationProgressText $nearPressureThreshold
Assert-True ($nearPressureProgress.Contains('missing=pressure-range>=0.050000,qualified-strokes=2')) `
    'pressure below threshold remains missing after tolerance'
$nearTiltXThreshold = New-StartedState
Add-TestStroke $nearTiltXThreshold 'near-tilt-x-threshold' 21 @(
    ,@(0,0,0.10,0,0.01,0.01),,@(0,0,0.15,1,0.0299996,0.03))
$nearTiltXProgress = Get-PenValidationProgressText $nearTiltXThreshold
Assert-True ($nearTiltXProgress.Contains('missing=tilt-x-range>=0.020000,qualified-strokes=2')) `
    'tilt X below threshold remains missing after tolerance'
$nearTiltYThreshold = New-StartedState
Add-TestStroke $nearTiltYThreshold 'near-tilt-y-threshold' 21 @(
    ,@(0,0,0.10,0,0.01,0.01),,@(0,0,0.15,1,0.03,0.0299996))
$nearTiltYProgress = Get-PenValidationProgressText $nearTiltYThreshold
Assert-True ($nearTiltYProgress.Contains('missing=tilt-y-range>=0.020000,qualified-strokes=2')) `
    'tilt Y below threshold remains missing after tolerance'

Add-TestStroke $progressState 'qualified-a' 24 @(
    ,@(0,0,0.20,0,-0.04,-0.03),,@(0,0,0.80,1,0.04,0.03))
$oneQualifiedProgress = Get-PenValidationProgressText $progressState
Assert-True ($oneQualifiedProgress.Contains('completed=2; qualified=1/2')) `
    'progress counts one qualified stroke'
Assert-True ($oneQualifiedProgress.EndsWith('missing=qualified-strokes=2')) `
    'progress asks for another qualified stroke'
Add-TestStroke $progressState 'qualified-b' 27 @(
    ,@(0,0,0.30,0,-0.03,0.04),,@(0,0,0.90,1,0.03,-0.04))
$passedProgress = Get-PenValidationProgressText $progressState
Assert-True ($passedProgress.Contains('completed=3; qualified=2/2')) `
    'progress counts all completed and qualified strokes'
Assert-True ($passedProgress.EndsWith('missing=none')) 'passed progress has no missing condition'

$valid = New-StartedState
Add-TestStroke $valid 'new-a' 21 @(,@(0.1,0.1,0.20,0,0.00,0.00),,@(0.2,0.2,0.50,5,0.04,-0.03))
Add-TestStroke $valid 'new-b' 24 @(,@(0.3,0.3,0.30,0,-0.03,0.04),,@(0.4,0.4,0.90,5,0.03,-0.04))
$validResult = Get-PenValidationResult $valid
Assert-True $validResult.Passed 'valid multi-stroke pen input'
Assert-True ($validResult.QualifiedPointCount -eq 4) 'baseline snapshot excluded'
Assert-True ($validResult.QualifiedPenStrokes -eq 2) 'both pen strokes qualify independently'

$mouse = New-StartedState
Add-TestStroke $mouse 'mouse-a' 21 @(,@(0.1,0.1,1.0,0,0.0,0.0))
Add-TestStroke $mouse 'mouse-b' 24 @(,@(0.2,0.2,1.0,1,0.0,0.0))
Assert-False (Get-PenValidationResult $mouse).Passed 'mouse fallback rejected'

$penAndMouse = New-StartedState
Add-TestStroke $penAndMouse 'pen' 21 @(,@(0,0,0.20,0,-0.04,-0.03),,@(0,0,0.80,1,0.04,0.03))
Add-TestStroke $penAndMouse 'mouse' 24 @(,@(0,0,1.0,0,0,0),,@(0,0,1.0,1,0,0))
$penAndMouseResult = Get-PenValidationResult $penAndMouse
Assert-False $penAndMouseResult.Passed 'one pen stroke plus mouse fallback rejected'
Assert-True ($penAndMouseResult.QualifiedPenStrokes -eq 1) 'only the pen stroke qualifies'
Assert-True ($penAndMouseResult.QualifiedPointCount -eq 2) 'fallback points excluded from evidence summary'

$cancelled = New-StartedState
Add-PenValidationMessage $cancelled ([pscustomobject]@{type='stroke_begin';rev=21;strokeId='cancelled';brush=(New-Brush)})
Add-PenValidationMessage $cancelled ([pscustomobject]@{type='stroke_points';rev=22;strokeId='cancelled';pts=@(,@(0,0,0.1,0,-0.5,-0.5),,@(0,0,0.9,1,0.5,0.5))})
Add-PenValidationMessage $cancelled ([pscustomobject]@{type='stroke_cancel';rev=23;strokeId='cancelled'})
Add-TestStroke $cancelled 'kept-a' 24 @(,@(0,0,0.50,0,0,0))
Add-TestStroke $cancelled 'kept-b' 27 @(,@(0,0,0.51,0,0,0))
$cancelledResult = Get-PenValidationResult $cancelled
Assert-False $cancelledResult.Passed 'cancelled dynamics excluded'
Assert-True ($cancelledResult.QualifiedPointCount -eq 0) 'cancelled dynamics do not qualify as evidence'

$singleJsonPoint = New-StartedState
Add-PenValidationMessage $singleJsonPoint '{"type":"stroke_begin","rev":21,"strokeId":"json","brush":{"tool":"marker","color":"#fff","opacity":0.5,"widthN":0.03,"pressureWidth":true,"pressureMin":0.65,"tiltWidth":true,"tiltMaxScale":1.75}}'
Add-PenValidationMessage $singleJsonPoint '{"type":"stroke_points","rev":22,"strokeId":"json","pts":[[0.1,0.2,0.3,0,0.04,-0.04]]}'
Add-PenValidationMessage $singleJsonPoint '{"type":"stroke_end","rev":23,"strokeId":"json","endedAt":1}'
Assert-True ((Get-PenValidationResult $singleJsonPoint).CompletedMarkerStrokes -eq 1) 'one-point JSON outer-array normalization'
$ps51UnwrappedPoint = New-StartedState
Add-PenValidationMessage $ps51UnwrappedPoint ([pscustomobject]@{type='stroke_begin';rev=21;strokeId='ps51';brush=(New-Brush)})
Add-PenValidationMessage $ps51UnwrappedPoint ([pscustomobject]@{type='stroke_points';rev=22;strokeId='ps51';pts=@(0.1,0.2,0.3,0,0.04,-0.04)})
Add-PenValidationMessage $ps51UnwrappedPoint ([pscustomobject]@{type='stroke_end';rev=23;strokeId='ps51';endedAt=1})
Assert-True ((Get-PenValidationResult $ps51UnwrappedPoint).CompletedMarkerStrokes -eq 1) 'PS5.1 unwrapped point normalization'

Assert-Throws {
    $postBaselineSnapshot = New-StartedState
    Add-PenValidationMessage $postBaselineSnapshot ([pscustomobject]@{type='stroke_begin';rev=21;strokeId='interrupted';brush=(New-Brush)})
    Add-PenValidationMessage $postBaselineSnapshot ([pscustomobject]@{type='snapshot';protocolVersion=7;rev=22;items=@()})
} 'unexpected snapshot after baseline' 'authoritative snapshot interrupts validation'
Assert-Throws {
    $revisionGap = New-StartedState
    Add-PenValidationMessage $revisionGap ([pscustomobject]@{type='stroke_begin';rev=22;strokeId='gap';brush=(New-Brush)})
} 'exactly consecutive' 'revision gaps rejected'

$malformed = New-StartedState
Assert-Throws {
    Add-PenValidationMessage $malformed ([pscustomobject]@{type='stroke_begin';rev=21;strokeId='bad';brush=(New-Brush)})
    Add-PenValidationMessage $malformed ([pscustomobject]@{type='stroke_points';rev=22;strokeId='bad';pts=@(,@(0,0,0,0,0))})
} 'exact 6-tuple' 'malformed tuple'
$outOfRange = New-StartedState
Assert-Throws {
    Add-PenValidationMessage $outOfRange ([pscustomobject]@{type='stroke_begin';rev=21;strokeId='bad';brush=(New-Brush)})
    Add-PenValidationMessage $outOfRange ([pscustomobject]@{type='stroke_points';rev=22;strokeId='bad';pts=@(,@(0,0,1.1,0,0,0))})
} 'outside the protocol range' 'out-of-range pressure'
Assert-Throws {
    $stringNumber = New-StartedState
    Add-PenValidationMessage $stringNumber '{"type":"stroke_begin","rev":21,"strokeId":"bad","brush":{"tool":"marker","pressureWidth":true,"pressureMin":0.65,"tiltWidth":true,"tiltMaxScale":1.75}}'
    Add-PenValidationMessage $stringNumber '{"type":"stroke_points","rev":22,"strokeId":"bad","pts":[[0,0,"0.5",0,0,0]]}'
} 'finite number' 'numeric string rejected'

$insufficientPressure = New-StartedState
Add-TestStroke $insufficientPressure 'p-a' 21 @(,@(0,0,0.50,0,-0.04,-0.03),,@(0,0,0.52,1,0.04,0.03))
Add-TestStroke $insufficientPressure 'p-b' 24 @(,@(0,0,0.50,0,-0.04,-0.03),,@(0,0,0.52,1,0.04,0.03))
Assert-False (Get-PenValidationResult $insufficientPressure).Passed 'insufficient pressure rejected'
$insufficientTiltX = New-StartedState
Add-TestStroke $insufficientTiltX 'x-a' 21 @(,@(0,0,0.2,0,0.00,-0.03),,@(0,0,0.8,1,0.01,0.03))
Add-TestStroke $insufficientTiltX 'x-b' 24 @(,@(0,0,0.2,0,0.00,-0.03),,@(0,0,0.8,1,0.01,0.03))
Assert-False (Get-PenValidationResult $insufficientTiltX).Passed 'insufficient tilt X rejected'
$insufficientTiltY = New-StartedState
Add-TestStroke $insufficientTiltY 'y-a' 21 @(,@(0,0,0.2,0,-0.03,0.00),,@(0,0,0.8,1,0.03,0.01))
Add-TestStroke $insufficientTiltY 'y-b' 24 @(,@(0,0,0.2,0,-0.03,0.00),,@(0,0,0.8,1,0.03,0.01))
Assert-False (Get-PenValidationResult $insufficientTiltY).Passed 'insufficient tilt Y rejected'
$constantTilt = New-StartedState
Add-TestStroke $constantTilt 'c-a' 21 @(,@(0,0,0.2,0,0.03,0.03),,@(0,0,0.8,1,0.03,0.03))
Add-TestStroke $constantTilt 'c-b' 24 @(,@(0,0,0.2,0,0.03,0.03),,@(0,0,0.8,1,0.03,0.03))
Assert-False (Get-PenValidationResult $constantTilt).Passed 'constant nonzero tilt rejected'

$sameBefore = @{ 'config.toml'=[pscustomobject]@{Exists=$true;Sha256='abc'};
    'config.toml.bak'=[pscustomobject]@{Exists=$false;Sha256=$null} }
$sameAfter = @{ 'config.toml'=[pscustomobject]@{Exists=$true;Sha256='abc'};
    'config.toml.bak'=[pscustomobject]@{Exists=$false;Sha256=$null} }
$changed = @{ 'config.toml'=[pscustomobject]@{Exists=$true;Sha256='def'};
    'config.toml.bak'=[pscustomobject]@{Exists=$false;Sha256=$null} }
Assert-True (Test-PenValidationConfigUnchanged $sameBefore $sameAfter) 'equal config hashes'
Assert-False (Test-PenValidationConfigUnchanged $sameBefore $changed) 'changed config hash rejected'

$expectedDevice = [pscustomobject][ordered]@{
    FriendlyName='Test Pen'; Class='HIDClass'; Status='OK'; DriverVersion='1.2.3';
    DriverProvider='Test Provider'
}
$validEnvironment = [pscustomobject]@{
    App=[pscustomobject]@{ListenerFound=$true;Version='0.7.0';Sha256=('ab' * 32)}
    OS=[pscustomobject]@{Caption='Windows';Version='10.0';Build='12345'}
    PresentPenDeviceCandidates=@($expectedDevice)
}
$verifiedDevice = Assert-PenValidationEnvironmentSummary $validEnvironment 'Test Pen'
Assert-True ($verifiedDevice.FriendlyName -ceq 'Test Pen') 'operator-confirmed device verified'
Assert-Throws {
    $missingListener = [pscustomobject]@{
        App=[pscustomobject]@{ListenerFound=$false;Version=$null;Sha256=$null}
        OS=$validEnvironment.OS
        PresentPenDeviceCandidates=@($expectedDevice)
    }
    Assert-PenValidationEnvironmentSummary $missingListener 'Test Pen'
} 'identify and hash' 'listener metadata is fail-closed'
Assert-Throws {
    $missingOs = [pscustomobject]@{
        App=$validEnvironment.App
        OS=[pscustomobject]@{Caption='Windows';Version=$null;Build=$null}
        PresentPenDeviceCandidates=@($expectedDevice)
    }
    Assert-PenValidationEnvironmentSummary $missingOs 'Test Pen'
} 'Windows version' 'OS metadata is fail-closed'
Assert-Throws {
    Assert-PenValidationEnvironmentSummary $validEnvironment 'Different Pen'
} 'expected pen device is not present' 'operator device name must match a present candidate'
$ambiguousEnvironment = [pscustomobject]@{
    App=$validEnvironment.App
    OS=$validEnvironment.OS
    PresentPenDeviceCandidates=@(
        $expectedDevice,
        [pscustomobject]@{FriendlyName='Test Pen';Class='USBDevice';Status='OK';
            DriverVersion='4.5.6';DriverProvider='Second Provider'}
    )
}
Assert-Throws {
    Assert-PenValidationEnvironmentSummary $ambiguousEnvironment 'Test Pen'
} 'ambiguous' 'duplicate device names are fail-closed'
$changedDevice = [pscustomobject][ordered]@{
    FriendlyName='Test Pen'; Class='HIDClass'; Status='OK'; DriverVersion='9.9.9';
    DriverProvider='Test Provider'
}
Assert-True (Test-PenValidationDeviceUnchanged $expectedDevice $expectedDevice) 'stable device metadata'
Assert-False (Test-PenValidationDeviceUnchanged $expectedDevice $changedDevice) 'changed driver rejected'
Assert-True (Test-PenValidationDeviceNameCandidate 'Wacom Intuos Pro L') 'Wacom model is a candidate'
Assert-True (Test-PenValidationDeviceNameCandidate 'Surface Pen') 'separate pen token is a candidate'
Assert-True (Test-PenValidationDeviceNameCandidate 'XP-PenTablet') 'common compound tablet name is a candidate'
$katakanaPen = ([string][char]0x30da) + ([string][char]0x30f3)
Assert-True (Test-PenValidationDeviceNameCandidate $katakanaPen) 'localized pen name is a candidate'
Assert-False (Test-PenValidationDeviceNameCandidate 'OpenVPN Adapter') 'pen substring is not a candidate'
Assert-False (Test-PenValidationDeviceNameCandidate 'Surface Camera') 'unrelated Surface device is not a candidate'

$tokens = $null
$errors = $null
$validatorAst = [Management.Automation.Language.Parser]::ParseFile(
    (Join-Path $PSScriptRoot 'validate-pen-input.ps1'), [ref]$tokens, [ref]$errors)
Assert-True ($errors.Count -eq 0) 'validator parses'
$entrypointParameters = @($validatorAst.ParamBlock.Parameters | ForEach-Object {
    $_.Name.VariablePath.UserPath
})
Assert-False ($entrypointParameters -contains 'ConfigDirectory') 'canonical config directory cannot be overridden'
$validatorText = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'validate-pen-input.ps1'))
$libraryText = [IO.File]::ReadAllText((Join-Path $PSScriptRoot 'pen-validation-lib.psm1'))
$readOnlyText = $validatorText + "`n" + $libraryText
Assert-True ($validatorText.Contains('ws://127.0.0.1:$Port/ws')) 'explicit loopback WebSocket URL'
Assert-True ($validatorText.Contains("SetRequestHeader('Origin', `$origin)")) 'exact Origin header'
Assert-True ($validatorText.Contains('[void]$client.ConnectAsync')) 'PS5.1 connect result stays out of JSON output'
Assert-True ($validatorText.Contains('ExpectedDeviceName')) 'operator-confirmed device is mandatory'
Assert-True ($validatorText.Contains('PublicDeviceLabel')) 'public device label is mandatory'
Assert-True ($validatorText.Contains('67108864')) 'protocol-sized snapshot cap'
Assert-True ($validatorText.Contains('Write-Host (Get-PenValidationProgressText -State $state)')) `
    'completed marker strokes emit sanitized progress'
Assert-True (([regex]::Matches(
    $validatorText, 'New-PenValidationTimeoutException -State \$state')).Count -eq 3) `
    'connect, deadline precheck, and receive cancellation include final progress'
Assert-True (([regex]::Matches(
    $validatorText, 'Test-PenValidationCancellationError -ErrorLike \$_')).Count -eq 2) `
    'connect and receive inspect wrapped cancellation causes'
Assert-True ($validatorText.Contains('catch [System.TimeoutException]')) `
    'timeout progress is emitted before fail-closed post-validation checks'
Assert-False ($validatorText -match '(?m)^\s*PresentPenDeviceCandidates\s*=') 'raw device candidates stay out of success JSON'
Assert-False ($readOnlyText -match '(?i)\b(?:Start|Stop)-Process\b') 'validator does not control app process'
Assert-False ($readOnlyText -match '(?i)\b(?:Set|Copy|Move|Remove)-(?:Item|Content)\b') 'validator does not write config or files'

if ($env:OS -ceq 'Windows_NT') {
    $sanitizedEnvironmentJson = Get-PenValidationEnvironmentSummary `
        -Port 1 `
        -ExpectedDeviceName '__StreamPainterNoSuchPen__' |
        ConvertTo-Json -Depth 5
    $privatePropertyPattern = `
        '(?i)"(?:PNPDeviceID|DeviceID|SerialNumber|UserName|ComputerName|FileName|Path)"\s*:'
    Assert-False ($sanitizedEnvironmentJson -match $privatePropertyPattern) `
        'environment summary excludes identifiers and paths'
}

Write-Host 'Pen validation helper tests passed.'
