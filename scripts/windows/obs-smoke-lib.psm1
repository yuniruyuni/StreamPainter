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
    'Get-AspectFitRect',
    'Test-StreamPainterSmokePixel',
    'Get-StreamPainterSmokeImageStatistics',
    'Assert-StreamPainterSmokeImage'
)
