$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'release-signing-lib.psm1') -Force

function Assert-Throws {
    param(
        [Parameter(Mandatory = $true)]
        [scriptblock] $Action,

        [Parameter(Mandatory = $true)]
        [string] $ExpectedMessage
    )

    $caught = $null
    try {
        & $Action
    } catch {
        $caught = $_.Exception
    }
    if ($null -eq $caught) {
        throw "Expected failure containing '$ExpectedMessage', but the action succeeded"
    }
    if ($caught.Message -notlike "*$ExpectedMessage*") {
        throw "Expected failure containing '$ExpectedMessage', got '$($caught.Message)'"
    }
}

function New-FakeCertificate {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Subject,

        [Parameter(Mandatory = $true)]
        [string] $Sha256
    )

    $certificate = [PSCustomObject]@{
        Subject = $Subject
        FakeSha256 = $Sha256
    }
    $certificate | Add-Member -MemberType ScriptMethod -Name GetCertHashString -Value {
        param($Algorithm)
        if ($Algorithm.Name -cne 'SHA256') {
            throw "Unexpected certificate hash algorithm: $($Algorithm.Name)"
        }
        return $this.FakeSha256
    }
    return $certificate
}

function New-FakeSignature {
    param(
        [string] $Status = 'Valid',
        $SignerCertificate,
        $TimeStamperCertificate = ([PSCustomObject]@{ Subject = 'CN=Test Timestamp Authority' })
    )

    return [PSCustomObject]@{
        Status = $Status
        StatusMessage = "test status: $Status"
        SignerCertificate = $SignerCertificate
        TimeStamperCertificate = $TimeStamperCertificate
    }
}

$expectedSubject = 'CN=Approved Test Publisher'
$expectedSha256 = ('ab' * 32)
$signer = New-FakeCertificate -Subject $expectedSubject -Sha256 $expectedSha256
$validSignature = New-FakeSignature -SignerCertificate $signer
$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) (
    'stream-painter-release-signing-test-' + [guid]::NewGuid().ToString('N')
)

try {
    $null = New-Item -ItemType Directory -Path $temporaryDirectory
    $inputPath = Join-Path $temporaryDirectory 'signed-input.exe'
    [IO.File]::WriteAllBytes($inputPath, [byte[]] (1, 2, 3, 4, 5))

    $signatureProvider = { param($Path) $validSignature }.GetNewClosure()
    $verificationState = @{ Calls = 0 }
    $signToolVerifier = {
        param($Path)
        $verificationState.Calls++
        if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
            throw 'Verifier did not receive the final asset path'
        }
    }.GetNewClosure()

    $signedOutput = Join-Path $temporaryDirectory 'signed-output'
    $assetName = 'StreamPainter-v1.2.3-windows-x64.exe'
    Complete-SignedReleaseAsset `
        -SignedPath $inputPath `
        -OutputDirectory $signedOutput `
        -AssetName $assetName `
        -ExpectedSignerSubject $expectedSubject `
        -ExpectedSignerSha256 $expectedSha256 `
        -SignatureProvider $signatureProvider `
        -SignToolVerifier $signToolVerifier

    if ($verificationState.Calls -ne 1) {
        throw "Expected one SignTool verification, got $($verificationState.Calls)"
    }
    Assert-ReleaseAssetBundle -Directory $signedOutput -AssetName $assetName
    $checksumPath = Join-Path $signedOutput "$assetName.sha256"
    $checksum = [IO.File]::ReadAllText($checksumPath)
    if ($checksum -notmatch '^[0-9a-f]{64}  StreamPainter-v1\.2\.3-windows-x64\.exe\r\n$') {
        throw "Checksum was not written in the expected post-signing format: $checksum"
    }

    [IO.File]::AppendAllText((Join-Path $signedOutput $assetName), 'tampered')
    Assert-Throws {
        Assert-ReleaseAssetBundle -Directory $signedOutput -AssetName $assetName
    } 'checksum mismatch'

    $invalidStatus = New-FakeSignature -Status 'HashMismatch' -SignerCertificate $signer
    $invalidProvider = { param($Path) $invalidStatus }.GetNewClosure()
    $failedOutput = Join-Path $temporaryDirectory 'failed-output'
    Assert-Throws {
        Complete-SignedReleaseAsset `
            -SignedPath $inputPath `
            -OutputDirectory $failedOutput `
            -AssetName $assetName `
            -ExpectedSignerSubject $expectedSubject `
            -ExpectedSignerSha256 $expectedSha256 `
            -SignatureProvider $invalidProvider `
            -SignToolVerifier $signToolVerifier
    } 'Authenticode status is HashMismatch'
    if (Test-Path -LiteralPath (Join-Path $failedOutput $assetName)) {
        throw 'A failed signature verification left a publishable executable behind'
    }
    if (Test-Path -LiteralPath (Join-Path $failedOutput "$assetName.sha256")) {
        throw 'A failed signature verification generated a checksum'
    }

    $missingTimestamp = New-FakeSignature -SignerCertificate $signer -TimeStamperCertificate $null
    $missingTimestampProvider = { param($Path) $missingTimestamp }.GetNewClosure()
    Assert-Throws {
        Assert-ReleaseAuthenticodeSignature `
            -Path $inputPath `
            -ExpectedSignerSubject $expectedSubject `
            -ExpectedSignerSha256 $expectedSha256 `
            -SignatureProvider $missingTimestampProvider `
            -SignToolVerifier $signToolVerifier
    } 'no timestamp countersignature'

    Assert-Throws {
        Assert-ReleaseAuthenticodeSignature `
            -Path $inputPath `
            -ExpectedSignerSubject 'CN=Wrong Publisher' `
            -ExpectedSignerSha256 $expectedSha256 `
            -SignatureProvider $signatureProvider `
            -SignToolVerifier $signToolVerifier
    } 'Unexpected signer subject'

    Assert-Throws {
        Assert-ReleaseAuthenticodeSignature `
            -Path $inputPath `
            -ExpectedSignerSubject $expectedSubject `
            -ExpectedSignerSha256 ('cd' * 32) `
            -SignatureProvider $signatureProvider `
            -SignToolVerifier $signToolVerifier
    } 'Unexpected signer certificate SHA-256'

    $failingSignTool = { param($Path) throw 'injected chain verification failure' }
    Assert-Throws {
        Assert-ReleaseAuthenticodeSignature `
            -Path $inputPath `
            -ExpectedSignerSubject $expectedSubject `
            -ExpectedSignerSha256 $expectedSha256 `
            -SignatureProvider $signatureProvider `
            -SignToolVerifier $failingSignTool
    } 'injected chain verification failure'

    if (Assert-SignPathToggle -Value '') {
        throw 'An absent SIGNPATH_ENABLED value must remain disabled'
    }
    if (-not (Assert-SignPathToggle -Value 'true')) {
        throw "SIGNPATH_ENABLED='true' must enable signing"
    }
    Assert-Throws { Assert-SignPathToggle -Value 'TRUE' } 'must be absent'
    Assert-Throws {
        Assert-SignPathConfiguration `
            -OrganizationId '' `
            -ProjectSlug 'project' `
            -SigningPolicySlug 'release' `
            -ArtifactConfigurationSlug 'artifact-v1' `
            -ExpectedSignerSubject $expectedSubject `
            -ExpectedSignerSha256 $expectedSha256 `
            -ApiTokenPresent $true
    } 'SIGNPATH_ORGANIZATION_ID is required'
    Assert-Throws {
        Assert-SignPathConfiguration `
            -OrganizationId 'organization' `
            -ProjectSlug 'project' `
            -SigningPolicySlug 'release' `
            -ArtifactConfigurationSlug 'artifact-v1' `
            -ExpectedSignerSubject $expectedSubject `
            -ExpectedSignerSha256 $expectedSha256 `
            -ApiTokenPresent $false
    } 'SIGNPATH_API_TOKEN is required'
    Assert-SignPathConfiguration `
        -OrganizationId 'organization' `
        -ProjectSlug 'project' `
        -SigningPolicySlug 'release' `
        -ArtifactConfigurationSlug 'artifact-v1' `
        -ExpectedSignerSubject $expectedSubject `
        -ExpectedSignerSha256 $expectedSha256 `
        -ApiTokenPresent $true

    $unsignedOutput = Join-Path $temporaryDirectory 'unsigned-output'
    Complete-UnsignedReleaseAsset `
        -UnsignedPath $inputPath `
        -OutputDirectory $unsignedOutput `
        -AssetName $assetName
    Assert-ReleaseAssetBundle -Directory $unsignedOutput -AssetName $assetName
    [IO.File]::WriteAllText((Join-Path $unsignedOutput 'unexpected.txt'), 'unexpected')
    Assert-Throws {
        Assert-ReleaseAssetBundle -Directory $unsignedOutput -AssetName $assetName
    } 'must contain exactly'

    if ($env:OS -ceq 'Windows_NT') {
        $signTool = Find-SignTool
        Assert-Throws {
            Invoke-SignToolVerification -Path $inputPath -SignToolPath $signTool
        } 'signtool verification failed'
        # The native failure is expected above, but pwsh otherwise propagates its stale exit code
        # after this successful test script finishes. Unexpected errors never reach this reset.
        $global:LASTEXITCODE = 0
    }
} finally {
    if (Test-Path -LiteralPath $temporaryDirectory) {
        Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force
    }
}

Write-Host 'release signing helper tests passed'
