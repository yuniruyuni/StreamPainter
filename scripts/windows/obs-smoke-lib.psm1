Set-StrictMode -Version Latest

function Assert-ObsArchiveIdentity {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Version,

        [Parameter(Mandatory = $true)]
        [uri]$ArchiveUri,

        [Parameter(Mandatory = $true)]
        [string]$Sha256
    )

    if ($Version -notmatch '^(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)$') {
        throw "OBS version is not a three-component release version: $Version"
    }
    if ($Sha256 -notmatch '^[0-9a-fA-F]{64}$') {
        throw "OBS archive SHA-256 is not 64 hexadecimal characters"
    }

    $archiveName = "OBS-Studio-$Version-Windows-x64.zip"
    $expectedUri = "https://github.com/obsproject/obs-studio/releases/download/$Version/$archiveName"
    if ($ArchiveUri.AbsoluteUri -cne $expectedUri) {
        throw "OBS archive URI must be the pinned official release asset: $expectedUri"
    }
}

function Get-ObsWebSocketAuthentication {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Password,

        [Parameter(Mandatory = $true)]
        [string]$Salt,

        [Parameter(Mandatory = $true)]
        [string]$Challenge
    )

    function Get-Sha256Base64([string]$Value) {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        try {
            $bytes = [System.Text.Encoding]::UTF8.GetBytes($Value)
            return [Convert]::ToBase64String($sha.ComputeHash($bytes))
        }
        finally {
            $sha.Dispose()
        }
    }

    $secret = Get-Sha256Base64 "$Password$Salt"
    return Get-Sha256Base64 "$secret$Challenge"
}

function Wait-ObsSmokeTask {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [System.Threading.Tasks.Task]$Task
    )

    # PowerShell exposes VoidTaskResult from non-generic Task.GetResult() on
    # its success stream. Suppress it so callers receive only their intended
    # WebSocket handle or payload.
    [void]$Task.GetAwaiter().GetResult()
}

function Get-AspectFitRect {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [double]$ScreenWidth,

        [Parameter(Mandatory = $true)]
        [double]$ScreenHeight,

        [double]$AspectWidth = 16.0,

        [double]$AspectHeight = 9.0
    )

    foreach ($value in @($ScreenWidth, $ScreenHeight, $AspectWidth, $AspectHeight)) {
        if ([double]::IsNaN($value) -or [double]::IsInfinity($value) -or $value -le 0.0) {
            throw "Aspect-fit dimensions must be positive finite values"
        }
    }

    $screenAspect = $ScreenWidth / $ScreenHeight
    $canvasAspect = $AspectWidth / $AspectHeight
    if ($screenAspect -gt $canvasAspect) {
        $width = $ScreenHeight * $canvasAspect
        return [pscustomobject]@{
            X = ($ScreenWidth - $width) / 2.0
            Y = 0.0
            Width = $width
            Height = $ScreenHeight
        }
    }

    $height = $ScreenWidth / $canvasAspect
    return [pscustomobject]@{
        X = 0.0
        Y = ($ScreenHeight - $height) / 2.0
        Width = $ScreenWidth
        Height = $height
    }
}

function ConvertTo-WindowsAbsoluteMouseCoordinate {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [double]$Position,

        [Parameter(Mandatory = $true)]
        [double]$Origin,

        [Parameter(Mandatory = $true)]
        [double]$Extent
    )

    foreach ($value in @($Position, $Origin, $Extent)) {
        if ([double]::IsNaN($value) -or [double]::IsInfinity($value)) {
            throw 'Absolute mouse coordinates must be finite values'
        }
    }
    if ($Extent -le 1.0) {
        throw 'Absolute mouse coordinate extent must be greater than one'
    }

    $offset = [Math]::Min([Math]::Max($Position - $Origin, 0.0), $Extent - 1.0)
    return [uint32][Math]::Round($offset * 65535.0 / ($Extent - 1.0))
}

function Get-ObjectPropertyValue {
    param(
        [AllowNull()][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name
    )

    if ($null -eq $Object) {
        return $null
    }
    $property = $Object.PSObject.Properties[$Name]
    if ($null -eq $property) {
        return $null
    }
    return $property.Value
}

function ConvertTo-FiniteDoubleOrNull {
    param([AllowNull()][object]$Value)

    if ($null -eq $Value) {
        return $null
    }
    try {
        $number = [double]$Value
    }
    catch {
        return $null
    }
    if ([double]::IsNaN($number) -or [double]::IsInfinity($number)) {
        return $null
    }
    return $number
}

function Get-StreamPainterSmokeStrokeDiagnostics {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [AllowNull()]
        [object]$Snapshot
    )

    $type = Get-ObjectPropertyValue -Object $Snapshot -Name 'type'
    $items = @(Get-ObjectPropertyValue -Object $Snapshot -Name 'items')
    $stroke = if ($items.Count -eq 1) { $items[0] } else { $null }
    $kind = Get-ObjectPropertyValue -Object $stroke -Name 'kind'
    $done = Get-ObjectPropertyValue -Object $stroke -Name 'done'
    $brush = Get-ObjectPropertyValue -Object $stroke -Name 'brush'
    $color = Get-ObjectPropertyValue -Object $brush -Name 'color'
    $rawPoints = @(Get-ObjectPropertyValue -Object $stroke -Name 'pts')

    $pointCount = 0
    $firstPoint = @()
    $lastPoint = @()
    if ($rawPoints.Count -gt 0) {
        # Windows PowerShell 5.1 unwraps a one-element outer JSON array. In
        # that case pts is the six scalar components of a single point.
        $firstScalar = ConvertTo-FiniteDoubleOrNull -Value $rawPoints[0]
        if ($null -ne $firstScalar) {
            $pointCount = 1
            $firstPoint = $rawPoints
            $lastPoint = $rawPoints
        }
        else {
            $pointCount = $rawPoints.Count
            $firstPoint = @($rawPoints[0])
            $lastPoint = @($rawPoints[-1])
        }
    }

    $firstU = if ($firstPoint.Count -ge 1) {
        ConvertTo-FiniteDoubleOrNull -Value $firstPoint[0]
    }
    else { $null }
    $firstV = if ($firstPoint.Count -ge 2) {
        ConvertTo-FiniteDoubleOrNull -Value $firstPoint[1]
    }
    else { $null }
    $lastU = if ($lastPoint.Count -ge 1) {
        ConvertTo-FiniteDoubleOrNull -Value $lastPoint[0]
    }
    else { $null }
    $lastV = if ($lastPoint.Count -ge 2) {
        ConvertTo-FiniteDoubleOrNull -Value $lastPoint[1]
    }
    else { $null }

    return [pscustomobject]@{
        Type = $type
        ItemCount = $items.Count
        Kind = $kind
        Done = $done
        Color = $color
        PointCount = $pointCount
        FirstU = $firstU
        FirstV = $firstV
        LastU = $lastU
        LastV = $lastV
    }
}

function Format-SmokeCoordinate {
    param([AllowNull()][object]$Value)

    if ($null -eq $Value) {
        return '<missing>'
    }
    return ([double]$Value).ToString('0.######', [Globalization.CultureInfo]::InvariantCulture)
}

function Format-StreamPainterSmokeStrokeDiagnostics {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [object]$Diagnostics
    )

    return (
        'type="{0}" items={1} kind="{2}" done={3} color="{4}" points={5} first=({6},{7}) last=({8},{9})' -f
            [string]$Diagnostics.Type,
            [int]$Diagnostics.ItemCount,
            [string]$Diagnostics.Kind,
            [string]$Diagnostics.Done,
            [string]$Diagnostics.Color,
            [int]$Diagnostics.PointCount,
            (Format-SmokeCoordinate -Value $Diagnostics.FirstU),
            (Format-SmokeCoordinate -Value $Diagnostics.FirstV),
            (Format-SmokeCoordinate -Value $Diagnostics.LastU),
            (Format-SmokeCoordinate -Value $Diagnostics.LastV)
    )
}

function Assert-StreamPainterSmokeSnapshot {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [AllowNull()]
        [object]$Snapshot
    )

    $diagnostics = Get-StreamPainterSmokeStrokeDiagnostics -Snapshot $Snapshot
    $summary = Format-StreamPainterSmokeStrokeDiagnostics -Diagnostics $diagnostics
    $coordinatesPresent = $null -ne $diagnostics.FirstU -and
        $null -ne $diagnostics.FirstV -and
        $null -ne $diagnostics.LastU -and
        $null -ne $diagnostics.LastV
    $coordinatesValid = $coordinatesPresent -and
        [Math]::Abs($diagnostics.FirstU - 0.25) -le 0.03 -and
        [Math]::Abs($diagnostics.FirstV - 0.50) -le 0.03 -and
        [Math]::Abs($diagnostics.LastU - 0.75) -le 0.03 -and
        [Math]::Abs($diagnostics.LastV - 0.50) -le 0.03 -and
        ($diagnostics.LastU - $diagnostics.FirstU) -ge 0.45 -and
        [Math]::Abs($diagnostics.LastV - $diagnostics.FirstV) -le 0.03

    if ([string]$diagnostics.Type -cne 'snapshot' -or
        $diagnostics.ItemCount -ne 1 -or
        [string]$diagnostics.Kind -cne 'stroke' -or
        $diagnostics.Done -ne $true -or
        [string]$diagnostics.Color -cne '#ff4d6d' -or
        $diagnostics.PointCount -lt 2 -or
        -not $coordinatesValid) {
        throw (
            "StreamPainter smoke stroke assertion failed: $summary; " +
            'expected a completed #ff4d6d stroke from approximately (0.25,0.50) to (0.75,0.50) ' +
            'with at least two points, horizontal span >= 0.45, and vertical drift <= 0.03'
        )
    }

    return $diagnostics
}

function Select-StreamPainterOverlayWindowRecord {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Windows,

        [Parameter(Mandatory = $true)]
        [uint32]$ProcessId,

        [Parameter(Mandatory = $true)]
        $Monitor,

        [ValidateRange(0, 16)]
        [int]$Tolerance = 2
    )

    $expectedLeft = [int]$Monitor.monitorPositionX
    $expectedTop = [int]$Monitor.monitorPositionY
    $expectedRight = $expectedLeft + [int]$Monitor.monitorWidth
    $expectedBottom = $expectedTop + [int]$Monitor.monitorHeight
    $candidates = @(
        foreach ($window in $Windows) {
            if ($null -eq $window -or [uint32]$window.ProcessId -ne $ProcessId) {
                continue
            }
            if ([string]$window.ClassName -cne 'stream-painter-overlay' -or
                [string]$window.Title -cne 'StreamPainter' -or
                $window.Visible -ne $true -or
                $window.Iconic -eq $true -or
                $window.RectValid -ne $true) {
                continue
            }
            if ([Math]::Abs([int]$window.Left - $expectedLeft) -gt $Tolerance -or
                [Math]::Abs([int]$window.Top - $expectedTop) -gt $Tolerance -or
                [Math]::Abs([int]$window.Right - $expectedRight) -gt $Tolerance -or
                [Math]::Abs([int]$window.Bottom - $expectedBottom) -gt $Tolerance) {
                continue
            }
            $window
        }
    )

    if ($candidates.Count -gt 1) {
        $handles = ($candidates | ForEach-Object { '0x{0:X}' -f [int64]$_.Hwnd }) -join ', '
        throw "Multiple visible StreamPainter overlay windows belong to PID $ProcessId`: $handles"
    }
    if ($candidates.Count -eq 1) {
        return $candidates[0]
    }
    return $null
}

function ConvertTo-DiagnosticText {
    param([AllowNull()][object]$Value)

    if ($null -eq $Value) {
        return ''
    }
    $text = [string]$Value
    $text = $text.Replace('\', '\\')
    $text = $text.Replace("`r", '\r')
    $text = $text.Replace("`n", '\n')
    $text = $text.Replace("`t", '\t')
    return $text.Replace('"', '\"')
}

function Format-TopLevelWindowDiagnostics {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [object[]]$Windows
    )

    $lines = [System.Collections.Generic.List[string]]::new()
    $lines.Add("top_level_window_count=$($Windows.Count)")
    $lines.Add('z_order is the EnumWindows order (zero is nearest the foreground)')
    foreach ($window in $Windows) {
        $session = if ([int]$window.SessionId -ge 0) { [string]$window.SessionId } else { '?' }
        $lines.Add((
            'z_order={0} hwnd=0x{1:X} pid={2} session={3} process="{4}" thread={5} desktop="{6}" class="{7}" title="{8}" style=0x{9:X8} exstyle=0x{10:X8} rect_valid={11} rect=({12},{13})-({14},{15}) visible={16} iconic={17}' -f
                [int]$window.ZOrder,
                [int64]$window.Hwnd,
                [uint32]$window.ProcessId,
                $session,
                (ConvertTo-DiagnosticText $window.ProcessName),
                [uint32]$window.ThreadId,
                (ConvertTo-DiagnosticText $window.Desktop),
                (ConvertTo-DiagnosticText $window.ClassName),
                (ConvertTo-DiagnosticText $window.Title),
                [uint32]$window.Style,
                [uint32]$window.ExtendedStyle,
                [bool]$window.RectValid,
                [int]$window.Left,
                [int]$window.Top,
                [int]$window.Right,
                [int]$window.Bottom,
                [bool]$window.Visible,
                [bool]$window.Iconic
        ))
    }
    return $lines -join [Environment]::NewLine
}

function Test-StreamPainterSmokePixel {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [int]$Red,

        [Parameter(Mandatory = $true)]
        [int]$Green,

        [Parameter(Mandatory = $true)]
        [int]$Blue,

        [Parameter(Mandatory = $true)]
        [int]$Alpha
    )

    # The smoke configuration uses #ff4d6d. Tolerances retain antialiased edge
    # pixels while excluding OBS error-page text and a black/transparent frame.
    return $Alpha -ge 96 -and $Red -ge 210 -and $Green -le 135 -and $Blue -le 165 -and
        ($Red - $Green) -ge 90 -and ($Red - $Blue) -ge 55
}

function Get-StreamPainterSmokeImageStatistics {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    Add-Type -AssemblyName System.Drawing
    $resolved = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).Path
    $bitmap = [System.Drawing.Bitmap]::new($resolved)
    try {
        $count = 0
        $minX = $bitmap.Width
        $maxX = -1
        $minY = $bitmap.Height
        $maxY = -1
        for ($y = 0; $y -lt $bitmap.Height; $y++) {
            for ($x = 0; $x -lt $bitmap.Width; $x++) {
                $pixel = $bitmap.GetPixel($x, $y)
                if (Test-StreamPainterSmokePixel -Red $pixel.R -Green $pixel.G -Blue $pixel.B -Alpha $pixel.A) {
                    $count++
                    if ($x -lt $minX) { $minX = $x }
                    if ($x -gt $maxX) { $maxX = $x }
                    if ($y -lt $minY) { $minY = $y }
                    if ($y -gt $maxY) { $maxY = $y }
                }
            }
        }

        return [pscustomobject]@{
            Width = $bitmap.Width
            Height = $bitmap.Height
            MatchingPixels = $count
            MinX = $minX
            MaxX = $maxX
            MinY = $minY
            MaxY = $maxY
        }
    }
    finally {
        $bitmap.Dispose()
    }
}

function Assert-StreamPainterSmokeImage {
    [CmdletBinding()]
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $statistics = Get-StreamPainterSmokeImageStatistics -Path $Path
    $horizontalSpan = if ($statistics.MaxX -ge 0) {
        $statistics.MaxX - $statistics.MinX + 1
    }
    else {
        0
    }
    $verticalSpan = if ($statistics.MaxY -ge 0) {
        $statistics.MaxY - $statistics.MinY + 1
    }
    else {
        0
    }
    $verticalCenter = if ($statistics.MaxY -ge 0) {
        ($statistics.MinY + $statistics.MaxY) / 2.0
    }
    else {
        -1.0
    }

    if ($statistics.Width -ne 640 -or $statistics.Height -ne 360) {
        throw "OBS Browser Source screenshot is $($statistics.Width)x$($statistics.Height), expected 640x360"
    }
    if ($statistics.MatchingPixels -lt 200) {
        throw "OBS Browser Source screenshot contains only $($statistics.MatchingPixels) expected-color pixels"
    }
    if ($horizontalSpan -lt [Math]::Floor($statistics.Width * 0.35)) {
        throw "Expected-color pixels span only $horizontalSpan horizontal pixels"
    }
    if ($verticalSpan -gt [Math]::Ceiling($statistics.Height * 0.25)) {
        throw "Expected-color pixels span an implausible $verticalSpan vertical pixels"
    }
    if ([Math]::Abs($verticalCenter - ($statistics.Height / 2.0)) -gt $statistics.Height * 0.15) {
        throw "Expected-color pixels are not centered on the injected stroke"
    }

    return $statistics
}

Export-ModuleMember -Function @(
    'Assert-ObsArchiveIdentity',
    'Get-ObsWebSocketAuthentication',
    'Wait-ObsSmokeTask',
    'Get-AspectFitRect',
    'ConvertTo-WindowsAbsoluteMouseCoordinate',
    'Get-StreamPainterSmokeStrokeDiagnostics',
    'Format-StreamPainterSmokeStrokeDiagnostics',
    'Assert-StreamPainterSmokeSnapshot',
    'Select-StreamPainterOverlayWindowRecord',
    'Format-TopLevelWindowDiagnostics',
    'Test-StreamPainterSmokePixel',
    'Get-StreamPainterSmokeImageStatistics',
    'Assert-StreamPainterSmokeImage'
)
