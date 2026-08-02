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
