Set-StrictMode -Version Latest

function Get-PenValidationProperty {
    param(
        [AllowNull()][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )

    if ($null -eq $Object) { return $null }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) { return $null }
    return $property.Value
}

function ConvertTo-PenValidationFiniteDouble {
    param(
        [AllowNull()][object]$Value,
        [Parameter(Mandatory = $true)][string]$Label
    )

    $isNumber = $Value -is [byte] -or $Value -is [sbyte] -or
        $Value -is [int16] -or $Value -is [uint16] -or
        $Value -is [int32] -or $Value -is [uint32] -or
        $Value -is [int64] -or $Value -is [uint64] -or
        $Value -is [single] -or $Value -is [double] -or $Value -is [decimal]
    if ($null -eq $Value -or -not $isNumber) {
        throw "$Label must be a finite number"
    }
    try { $number = [double]$Value }
    catch { throw "$Label must be a finite number" }
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
        throw "$Label must be a finite number"
    }
    return $number
}

function Test-PenValidationBoolean {
    param([AllowNull()][object]$Value, [bool]$Expected)
    return ($Value -is [bool] -and $Value -eq $Expected)
}

function Test-PenValidationMinimumThreshold {
    param(
        [Parameter(Mandatory = $true)][double]$Value,
        [Parameter(Mandatory = $true)][double]$Minimum
    )

    # Decimal protocol values such as 0.15 - 0.10 may land a few ULP below the
    # mathematical threshold. This tolerance accepts that representation error,
    # while remaining far below a meaningful pen measurement difference.
    return $Value -ge ($Minimum - 0.000000000001)
}

function Test-PenValidationCancellationError {
    [CmdletBinding()]
    param([AllowNull()][object]$ErrorLike)

    if ($null -eq $ErrorLike) { return $false }
    $candidate = if ($ErrorLike -is [Management.Automation.ErrorRecord]) {
        $ErrorLike.Exception
    } else {
        $ErrorLike
    }
    # Windows PowerShell may wrap a method's OperationCanceledException in one or
    # more MethodInvocationException/RuntimeException layers. Inspect types only;
    # never classify an unrelated error from its message or FullyQualifiedErrorId.
    for ($depth = 0; $depth -lt 16 -and $null -ne $candidate; $depth++) {
        if ($candidate -is [System.OperationCanceledException]) { return $true }
        $next = Get-PenValidationProperty $candidate 'InnerException'
        if ($null -eq $next -or [object]::ReferenceEquals($candidate, $next)) { break }
        $candidate = $next
    }
    return $false
}

function Assert-PenValidationMarkerBrush {
    param([Parameter(Mandatory = $true)][object]$Brush)

    if ((Get-PenValidationProperty $Brush 'tool') -cne 'marker') {
        throw 'marker stroke brush.tool must be marker'
    }
    if (-not (Test-PenValidationBoolean (Get-PenValidationProperty $Brush 'pressureWidth') $true)) {
        throw 'marker stroke brush.pressureWidth must be true'
    }
    if (-not (Test-PenValidationBoolean (Get-PenValidationProperty $Brush 'tiltWidth') $true)) {
        throw 'marker stroke brush.tiltWidth must be true'
    }
    $pressureMin = ConvertTo-PenValidationFiniteDouble `
        (Get-PenValidationProperty $Brush 'pressureMin') 'marker stroke brush.pressureMin'
    $tiltMaxScale = ConvertTo-PenValidationFiniteDouble `
        (Get-PenValidationProperty $Brush 'tiltMaxScale') 'marker stroke brush.tiltMaxScale'
    if ([Math]::Abs($pressureMin - 0.65) -gt 0.0000001) {
        throw 'marker stroke brush.pressureMin must be 0.65'
    }
    if ([Math]::Abs($tiltMaxScale - 1.75) -gt 0.0000001) {
        throw 'marker stroke brush.tiltMaxScale must be 1.75'
    }
}

function New-PenValidationState {
    [CmdletBinding()]
    param()

    return [pscustomobject]@{
        BaselineReceived = $false
        ProtocolVersion = $null
        BaselineRevision = [int64]-1
        LastRevision = [int64]-1
        ActiveStrokes = @{}
        CompletedStrokes = 0
        QualifiedStrokes = 0
        QualifiedPointCount = 0
        Pressures = New-Object System.Collections.ArrayList
        TiltXs = New-Object System.Collections.ArrayList
        TiltYs = New-Object System.Collections.ArrayList
        LastCompletedStroke = $null
    }
}

function Add-PenValidationMessage {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][object]$State,
        [Parameter(Mandatory = $true)][object]$Message
    )

    if ($Message -is [string]) {
        try { $Message = $Message | ConvertFrom-Json }
        catch { throw 'WebSocket message is not valid JSON' }
    }
    $type = Get-PenValidationProperty $Message 'type'
    if (-not $State.BaselineReceived) {
        if ($type -cne 'snapshot') {
            throw 'first WebSocket message must be the baseline snapshot'
        }
        $protocol = ConvertTo-PenValidationFiniteDouble `
            (Get-PenValidationProperty $Message 'protocolVersion') 'snapshot.protocolVersion'
        if ($protocol -ne 6 -and $protocol -ne 7) {
            throw 'snapshot protocolVersion must be 6 or 7'
        }
        $revision = ConvertTo-PenValidationFiniteDouble `
            (Get-PenValidationProperty $Message 'rev') 'snapshot.rev'
        if ($revision -lt 0 -or [Math]::Floor($revision) -ne $revision) {
            throw 'snapshot.rev must be a non-negative integer'
        }
        $State.BaselineReceived = $true
        $State.ProtocolVersion = [int]$protocol
        $State.BaselineRevision = [int64]$revision
        $State.LastRevision = [int64]$revision
        return
    }

    if ($type -eq 'snapshot') {
        throw 'unexpected snapshot after baseline; restart pen validation'
    }
    if ($type -eq 'pong') { return }
    $revision = ConvertTo-PenValidationFiniteDouble `
        (Get-PenValidationProperty $Message 'rev') 'event.rev'
    if ([Math]::Floor($revision) -ne $revision -or
        $revision -ne ($State.LastRevision + 1)) {
        throw 'incremental event revision must be exactly consecutive'
    }
    $State.LastRevision = [int64]$revision

    $strokeId = Get-PenValidationProperty $Message 'strokeId'
    switch ($type) {
        'stroke_begin' {
            if (-not ($strokeId -is [string]) -or [string]::IsNullOrWhiteSpace($strokeId)) {
                throw 'stroke_begin.strokeId must be a non-empty string'
            }
            $brush = Get-PenValidationProperty $Message 'brush'
            if ($null -eq $brush) { throw 'stroke_begin.brush is required' }
            if ((Get-PenValidationProperty $brush 'tool') -cne 'marker') { return }
            Assert-PenValidationMarkerBrush $brush
            if ($State.ActiveStrokes.ContainsKey($strokeId)) {
                throw 'duplicate marker stroke_begin'
            }
            $State.ActiveStrokes[$strokeId] = [pscustomobject]@{
                PointCount = 0
                Pressures = New-Object System.Collections.ArrayList
                TiltXs = New-Object System.Collections.ArrayList
                TiltYs = New-Object System.Collections.ArrayList
            }
        }
        'stroke_points' {
            if (-not ($strokeId -is [string]) -or -not $State.ActiveStrokes.ContainsKey($strokeId)) {
                return
            }
            $pointsValue = Get-PenValidationProperty $Message 'pts'
            if ($null -eq $pointsValue) { throw 'stroke_points.pts is required' }
            $points = @($pointsValue)
            # Windows PowerShell 5.1 unwraps a one-element outer JSON array.
            # Six scalar values are therefore the single point, not six points.
            if ($points.Count -eq 6) {
                $allScalars = $true
                foreach ($candidate in $points) {
                    if ($candidate -is [array] -or $candidate -is [pscustomobject]) {
                        $allScalars = $false
                    }
                }
                if ($allScalars) { $points = @(,$points) }
            }
            if ($points.Count -eq 0) { throw 'stroke_points.pts must not be empty' }
            foreach ($pointValue in $points) {
                $point = @($pointValue)
                while ($point.Count -eq 1 -and $point[0] -is [array]) {
                    $point = @($point[0])
                }
                if ($point.Count -ne 6) { throw 'each marker point must be an exact 6-tuple' }
                $values = @()
                for ($index = 0; $index -lt 6; $index++) {
                    $values += ConvertTo-PenValidationFiniteDouble $point[$index] "point[$index]"
                }
                if ($values[0] -lt 0 -or $values[0] -gt 1 -or
                    $values[1] -lt 0 -or $values[1] -gt 1 -or
                    $values[2] -lt 0 -or $values[2] -gt 1 -or $values[3] -lt 0 -or
                    $values[4] -lt -1 -or $values[4] -gt 1 -or
                    $values[5] -lt -1 -or $values[5] -gt 1) {
                    throw 'marker point component is outside the protocol range'
                }
                $active = $State.ActiveStrokes[$strokeId]
                [void]$active.Pressures.Add($values[2])
                [void]$active.TiltXs.Add($values[4])
                [void]$active.TiltYs.Add($values[5])
                $active.PointCount++
            }
        }
        'stroke_end' {
            if (-not ($strokeId -is [string]) -or -not $State.ActiveStrokes.ContainsKey($strokeId)) {
                return
            }
            if ($State.ActiveStrokes[$strokeId].PointCount -eq 0) {
                throw 'marker stroke ended without points'
            }
            $active = $State.ActiveStrokes[$strokeId]
            $State.ActiveStrokes.Remove($strokeId)
            $State.CompletedStrokes++
            $strokeResult = Get-PenValidationDynamicsResult `
                -PointCount $active.PointCount `
                -Pressures $active.Pressures `
                -TiltXs $active.TiltXs `
                -TiltYs $active.TiltYs
            $State.LastCompletedStroke = [pscustomobject][ordered]@{
                PointCount = $active.PointCount
                Qualified = [bool]$strokeResult.Passed
                PressureRange = [double]$strokeResult.PressureRange
                TiltXRange = [double]$strokeResult.TiltXRange
                TiltYRange = [double]$strokeResult.TiltYRange
            }
            if ($strokeResult.Passed) {
                foreach ($value in $active.Pressures) { [void]$State.Pressures.Add($value) }
                foreach ($value in $active.TiltXs) { [void]$State.TiltXs.Add($value) }
                foreach ($value in $active.TiltYs) { [void]$State.TiltYs.Add($value) }
                $State.QualifiedPointCount += $active.PointCount
                $State.QualifiedStrokes++
            }
        }
        'stroke_cancel' {
            if ($strokeId -is [string] -and $State.ActiveStrokes.ContainsKey($strokeId)) {
                $State.ActiveStrokes.Remove($strokeId)
            }
        }
    }
}

function Get-PenValidationDynamicsResult {
    param(
        [Parameter(Mandatory = $true)][int]$PointCount,
        [Parameter(Mandatory = $true)][object]$Pressures,
        [Parameter(Mandatory = $true)][object]$TiltXs,
        [Parameter(Mandatory = $true)][object]$TiltYs
    )

    $pressureRange = 0.0
    $tiltXRange = 0.0
    $tiltYRange = 0.0
    $tiltXMagnitude = 0.0
    $tiltYMagnitude = 0.0
    if ($PointCount -gt 0) {
        $pressureRange = ($Pressures | Measure-Object -Maximum).Maximum -
            ($Pressures | Measure-Object -Minimum).Minimum
        $tiltXRange = ($TiltXs | Measure-Object -Maximum).Maximum -
            ($TiltXs | Measure-Object -Minimum).Minimum
        $tiltYRange = ($TiltYs | Measure-Object -Maximum).Maximum -
            ($TiltYs | Measure-Object -Minimum).Minimum
        foreach ($value in $TiltXs) { $tiltXMagnitude = [Math]::Max($tiltXMagnitude, [Math]::Abs($value)) }
        foreach ($value in $TiltYs) { $tiltYMagnitude = [Math]::Max($tiltYMagnitude, [Math]::Abs($value)) }
    }
    return [pscustomobject]@{
        Passed = $PointCount -ge 2 -and
            (Test-PenValidationMinimumThreshold $pressureRange 0.05) -and
            (Test-PenValidationMinimumThreshold $tiltXRange 0.02) -and
            (Test-PenValidationMinimumThreshold $tiltYRange 0.02)
        PressureRange = $pressureRange
        TiltXRange = $tiltXRange
        TiltYRange = $tiltYRange
        TiltXMagnitude = $tiltXMagnitude
        TiltYMagnitude = $tiltYMagnitude
    }
}

function Get-PenValidationResult {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][object]$State)

    $pressureRange = 0.0
    $tiltXRange = 0.0
    $tiltYRange = 0.0
    $tiltXMagnitude = 0.0
    $tiltYMagnitude = 0.0
    if ($State.QualifiedPointCount -gt 0) {
        $pressureRange = ($State.Pressures | Measure-Object -Maximum).Maximum -
            ($State.Pressures | Measure-Object -Minimum).Minimum
        $tiltXRange = ($State.TiltXs | Measure-Object -Maximum).Maximum -
            ($State.TiltXs | Measure-Object -Minimum).Minimum
        $tiltYRange = ($State.TiltYs | Measure-Object -Maximum).Maximum -
            ($State.TiltYs | Measure-Object -Minimum).Minimum
        foreach ($value in $State.TiltXs) { $tiltXMagnitude = [Math]::Max($tiltXMagnitude, [Math]::Abs($value)) }
        foreach ($value in $State.TiltYs) { $tiltYMagnitude = [Math]::Max($tiltYMagnitude, [Math]::Abs($value)) }
    }
    return [pscustomobject]@{
        Passed = $State.QualifiedStrokes -ge 2
        ProtocolVersion = $State.ProtocolVersion
        CompletedMarkerStrokes = $State.CompletedStrokes
        QualifiedPenStrokes = $State.QualifiedStrokes
        QualifiedPointCount = $State.QualifiedPointCount
        PressureRange = [Math]::Round($pressureRange, 6)
        TiltXRange = [Math]::Round($tiltXRange, 6)
        TiltYRange = [Math]::Round($tiltYRange, 6)
        TiltXMagnitude = [Math]::Round($tiltXMagnitude, 6)
        TiltYMagnitude = [Math]::Round($tiltYMagnitude, 6)
    }
}

function ConvertTo-PenValidationProgressNumber {
    param([Parameter(Mandatory = $true)][double]$Value)

    return ([Math]::Round($Value, 6)).ToString(
        '0.000000', [Globalization.CultureInfo]::InvariantCulture)
}

function Get-PenValidationProgressText {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][object]$State)

    $missing = New-Object System.Collections.Generic.List[string]
    $last = $State.LastCompletedStroke
    if ($null -eq $last) {
        $lastText = 'last-stroke=none'
        [void]$missing.Add('completed-marker-stroke')
    }
    else {
        $pointCount = [int]$last.PointCount
        $pressureRange = [double]$last.PressureRange
        $tiltXRange = [double]$last.TiltXRange
        $tiltYRange = [double]$last.TiltYRange
        if ($pointCount -lt 2) { [void]$missing.Add('point-count>=2') }
        if (-not (Test-PenValidationMinimumThreshold $pressureRange 0.05)) {
            [void]$missing.Add('pressure-range>=0.050000')
        }
        if (-not (Test-PenValidationMinimumThreshold $tiltXRange 0.02)) {
            [void]$missing.Add('tilt-x-range>=0.020000')
        }
        if (-not (Test-PenValidationMinimumThreshold $tiltYRange 0.02)) {
            [void]$missing.Add('tilt-y-range>=0.020000')
        }
        $lastQualified = ([bool]$last.Qualified).ToString().ToLowerInvariant()
        $lastText = 'last-points={0}; last-pressure-range={1}; last-tilt-x-range={2}; ' +
            'last-tilt-y-range={3}; last-qualified={4}'
        $lastText = $lastText -f $pointCount,
            (ConvertTo-PenValidationProgressNumber $pressureRange),
            (ConvertTo-PenValidationProgressNumber $tiltXRange),
            (ConvertTo-PenValidationProgressNumber $tiltYRange),
            $lastQualified
    }
    if ($State.QualifiedStrokes -lt 2) { [void]$missing.Add('qualified-strokes=2') }
    $missingText = if ($missing.Count -eq 0) { 'none' } else { $missing -join ',' }
    return 'Pen validation progress: completed={0}; qualified={1}/2; {2}; missing={3}' -f
        $State.CompletedStrokes, $State.QualifiedStrokes, $lastText, $missingText
}

function New-PenValidationTimeoutException {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][object]$State)

    $message = 'pen validation timed out before all thresholds were met. ' +
        (Get-PenValidationProgressText -State $State)
    return (New-Object System.TimeoutException -ArgumentList @(,$message))
}

function Get-PenValidationConfigState {
    [CmdletBinding()]
    param([Parameter(Mandatory = $true)][string]$ConfigDirectory)

    $state = @{}
    foreach ($name in @('config.toml', 'config.toml.bak')) {
        $path = Join-Path $ConfigDirectory $name
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            $state[$name] = [pscustomobject]@{
                Exists = $true
                Sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
            }
        }
        else {
            $state[$name] = [pscustomobject]@{ Exists = $false; Sha256 = $null }
        }
    }
    return $state
}

function Test-PenValidationConfigUnchanged {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][hashtable]$Before,
        [Parameter(Mandatory = $true)][hashtable]$After
    )

    foreach ($name in @('config.toml', 'config.toml.bak')) {
        if ($Before[$name].Exists -ne $After[$name].Exists -or
            $Before[$name].Sha256 -cne $After[$name].Sha256) { return $false }
    }
    return $true
}

function ConvertTo-PenValidationSafeText {
    param([AllowNull()][object]$Value)
    if ($null -eq $Value) { return $null }
    $text = ([string]$Value) -replace '[\x00-\x1f\x7f]', ' '
    $text = $text.Trim()
    if ($text.Length -gt 160) { $text = $text.Substring(0, 160) }
    return $text
}

function Test-PenValidationDeviceNameCandidate {
    param([AllowNull()][object]$Name)
    return ($Name -is [string] -and
        $Name -match '(?i)(?:\b(?:pen|stylus|tablet|digitizer|wacom)\b|pentablet|\u30da\u30f3|\u30bf\u30d6\u30ec\u30c3\u30c8|\u30c7\u30b8\u30bf\u30a4\u30b6)')
}

function Get-PenValidationEnvironmentSummary {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][ValidateRange(1, 65535)][int]$Port,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedDeviceName
    )

    $osSummary = [ordered]@{ Caption = $null; Version = [Environment]::OSVersion.VersionString; Build = $null }
    $deviceCandidates = @()
    $services = @()
    $appSummary = [ordered]@{
        ListenerFound = $false
        VersionAvailable = $false
        Version = $null
        Sha256 = $null
    }
    if ($env:OS -eq 'Windows_NT') {
        try {
            $os = Get-CimInstance Win32_OperatingSystem -ErrorAction Stop
            $osSummary.Caption = ConvertTo-PenValidationSafeText $os.Caption
            $osSummary.Version = ConvertTo-PenValidationSafeText $os.Version
            $osSummary.Build = ConvertTo-PenValidationSafeText $os.BuildNumber
        } catch {}
        try {
            $signedDrivers = @(Get-CimInstance Win32_PnPSignedDriver -ErrorAction Stop)
            $deviceCandidates = @(Get-CimInstance Win32_PnPEntity -ErrorAction Stop | Where-Object {
                $_.Present -eq $true -and (
                    (Test-PenValidationDeviceNameCandidate $_.Name) -or
                    [string]::Equals($_.Name, $ExpectedDeviceName, [StringComparison]::OrdinalIgnoreCase)
                )
            } | ForEach-Object {
                $entity = $_
                $driver = $signedDrivers | Where-Object {
                    $_.DeviceID -eq $entity.PNPDeviceID
                } | Select-Object -First 1
                [pscustomobject][ordered]@{
                    FriendlyName = ConvertTo-PenValidationSafeText $entity.Name
                    Class = ConvertTo-PenValidationSafeText $entity.PNPClass
                    Status = ConvertTo-PenValidationSafeText $entity.Status
                    DriverVersion = ConvertTo-PenValidationSafeText `
                        (Get-PenValidationProperty $driver 'DriverVersion')
                    DriverProvider = ConvertTo-PenValidationSafeText `
                        (Get-PenValidationProperty $driver 'DriverProviderName')
                }
            } | Sort-Object FriendlyName, Class, Status, DriverVersion, DriverProvider)
        } catch {}
        try {
            $services = @(Get-Service -Name 'WTablet*' -ErrorAction SilentlyContinue | ForEach-Object {
                [pscustomobject][ordered]@{
                    Name = ConvertTo-PenValidationSafeText $_.Name
                    Status = ConvertTo-PenValidationSafeText $_.Status
                }
            })
        } catch {}
        try {
            $listener = Get-NetTCPConnection -LocalAddress '127.0.0.1' -LocalPort $Port `
                -State Listen -ErrorAction Stop | Select-Object -First 1
            $process = Get-Process -Id $listener.OwningProcess -ErrorAction Stop
            $path = $process.MainModule.FileName
            $versionInfo = [Diagnostics.FileVersionInfo]::GetVersionInfo($path)
            $appSummary.ListenerFound = $true
            $appSummary.Version = ConvertTo-PenValidationSafeText $versionInfo.FileVersion
            $appSummary.VersionAvailable = -not [string]::IsNullOrWhiteSpace($appSummary.Version)
            $appSummary.Sha256 = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash.ToLowerInvariant()
        } catch {}
    }
    return [pscustomobject][ordered]@{
        OS = [pscustomobject]$osSummary
        PresentPenDeviceCandidates = $deviceCandidates
        TabletServices = $services
        App = [pscustomobject]$appSummary
    }
}

function Assert-PenValidationEnvironmentSummary {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][object]$Environment,
        [Parameter(Mandatory = $true)][ValidateNotNullOrEmpty()][string]$ExpectedDeviceName
    )

    if (-not (Test-PenValidationBoolean $Environment.App.ListenerFound $true) -or
        -not ($Environment.App.Sha256 -is [string]) -or
        $Environment.App.Sha256 -cnotmatch '^[0-9a-f]{64}$') {
        throw 'could not identify and hash the app that owns the requested loopback listener'
    }
    foreach ($name in @('Caption', 'Version', 'Build')) {
        $value = Get-PenValidationProperty $Environment.OS $name
        if (-not ($value -is [string]) -or [string]::IsNullOrWhiteSpace($value)) {
            throw 'could not identify the Windows version used for pen validation'
        }
    }
    $matches = @($Environment.PresentPenDeviceCandidates | Where-Object {
        [string]::Equals($_.FriendlyName, $ExpectedDeviceName, [StringComparison]::OrdinalIgnoreCase)
    })
    if ($matches.Count -eq 0) {
        throw 'expected pen device is not present in the sanitized device candidates'
    }
    if ($matches.Count -ne 1) {
        throw 'expected pen device name is ambiguous; use a unique present device name'
    }
    $verified = @($matches | Where-Object {
        [string]::Equals($_.Status, 'OK', [StringComparison]::OrdinalIgnoreCase) -and
        $_.DriverVersion -is [string] -and -not [string]::IsNullOrWhiteSpace($_.DriverVersion) -and
        $_.DriverProvider -is [string] -and -not [string]::IsNullOrWhiteSpace($_.DriverProvider)
    }) | Select-Object -First 1
    if ($null -eq $verified) {
        throw 'expected pen device is not healthy or has no attributable driver metadata'
    }
    return $verified
}

function Test-PenValidationDeviceUnchanged {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)][object]$Before,
        [Parameter(Mandatory = $true)][object]$After
    )

    foreach ($name in @('FriendlyName', 'Class', 'Status', 'DriverVersion', 'DriverProvider')) {
        if ((Get-PenValidationProperty $Before $name) -cne (Get-PenValidationProperty $After $name)) {
            return $false
        }
    }
    return $true
}

Export-ModuleMember -Function New-PenValidationState, Add-PenValidationMessage, `
    Get-PenValidationResult, Get-PenValidationProgressText, `
    New-PenValidationTimeoutException, Get-PenValidationConfigState, `
    Test-PenValidationConfigUnchanged, Get-PenValidationEnvironmentSummary, `
    Assert-PenValidationEnvironmentSummary, Test-PenValidationDeviceUnchanged, `
    Test-PenValidationDeviceNameCandidate, Test-PenValidationCancellationError
