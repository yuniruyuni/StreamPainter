Set-StrictMode -Version Latest

function ConvertTo-NormalizedCertificateSha256 {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Value
    )

    $normalized = ($Value -replace '[:\s-]', '').ToLowerInvariant()
    if ($normalized -notmatch '^[0-9a-f]{64}$') {
        throw "Certificate SHA-256 must contain exactly 64 hexadecimal characters"
    }
    return $normalized
}

function Assert-SignPathToggle {
    param(
        [AllowEmptyString()]
        [string] $Value = ''
    )

    if ($Value -cnotin @('', 'false', 'true')) {
        throw "SIGNPATH_ENABLED must be absent, 'false', or 'true' (lowercase); got '$Value'"
    }
    return $Value -ceq 'true'
}

function Assert-ConfiguredValue {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Name,

        [AllowEmptyString()]
        [string] $Value
    )

    if ([string]::IsNullOrWhiteSpace($Value)) {
        throw "$Name is required when SignPath signing is enabled"
    }
    if ($Value.Contains('<') -or $Value.Contains('>') -or $Value -match '^(?i:todo|changeme|placeholder)$') {
        throw "$Name still contains a placeholder"
    }
}

function Assert-SignPathConfiguration {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $OrganizationId,

        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $ProjectSlug,

        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $SigningPolicySlug,

        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $ArtifactConfigurationSlug,

        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $ExpectedSignerSubject,

        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string] $ExpectedSignerSha256,

        [Parameter(Mandatory = $true)]
        [bool] $ApiTokenPresent
    )

    Assert-ConfiguredValue -Name 'SIGNPATH_ORGANIZATION_ID' -Value $OrganizationId
    Assert-ConfiguredValue -Name 'SIGNPATH_PROJECT_SLUG' -Value $ProjectSlug
    Assert-ConfiguredValue -Name 'SIGNPATH_SIGNING_POLICY_SLUG' -Value $SigningPolicySlug
    Assert-ConfiguredValue -Name 'SIGNPATH_ARTIFACT_CONFIGURATION_SLUG' -Value $ArtifactConfigurationSlug
    Assert-ConfiguredValue -Name 'SIGNPATH_EXPECTED_SIGNER_SUBJECT' -Value $ExpectedSignerSubject
    $null = ConvertTo-NormalizedCertificateSha256 -Value $ExpectedSignerSha256
    if (-not $ApiTokenPresent) {
        throw 'The code-signing environment secret SIGNPATH_API_TOKEN is required'
    }
}

function Find-SignTool {
    $configured = [Environment]::GetEnvironmentVariable('SIGNTOOL_PATH')
    if (-not [string]::IsNullOrWhiteSpace($configured)) {
        if (-not (Test-Path -LiteralPath $configured -PathType Leaf)) {
            throw "SIGNTOOL_PATH does not point to a file: $configured"
        }
        return (Resolve-Path -LiteralPath $configured).ProviderPath
    }

    $command = Get-Command signtool.exe -CommandType Application -ErrorAction SilentlyContinue |
        Select-Object -First 1
    if ($null -ne $command) {
        return $command.Source
    }

    $roots = @(
        [Environment]::GetEnvironmentVariable('ProgramFiles(x86)'),
        [Environment]::GetEnvironmentVariable('ProgramFiles')
    ) | Where-Object { -not [string]::IsNullOrWhiteSpace($_) }

    $candidates = foreach ($root in $roots) {
        $sdkBin = Join-Path $root 'Windows Kits\10\bin'
        if (Test-Path -LiteralPath $sdkBin -PathType Container) {
            Get-ChildItem -LiteralPath $sdkBin -Directory -ErrorAction SilentlyContinue |
                ForEach-Object {
                    $candidate = Join-Path $_.FullName 'x64\signtool.exe'
                    if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                        Get-Item -LiteralPath $candidate
                    }
                }
            $unversioned = Join-Path $sdkBin 'x64\signtool.exe'
            if (Test-Path -LiteralPath $unversioned -PathType Leaf) {
                Get-Item -LiteralPath $unversioned
            }
        }
    }

    $selected = $candidates | Sort-Object FullName -Descending | Select-Object -First 1
    if ($null -eq $selected) {
        throw 'signtool.exe was not found in PATH or an installed Windows 10/11 SDK'
    }
    return $selected.FullName
}

function Invoke-SignToolVerification {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,

        [string] $SignToolPath
    )

    $tool = if ([string]::IsNullOrWhiteSpace($SignToolPath)) {
        Find-SignTool
    } else {
        if (-not (Test-Path -LiteralPath $SignToolPath -PathType Leaf)) {
            throw "signtool.exe was not found: $SignToolPath"
        }
        (Resolve-Path -LiteralPath $SignToolPath).ProviderPath
    }

    # Windows PowerShell converts native stderr to ErrorRecord. Capture it and decide from
    # the native exit code so callers always get one fail-closed error shape.
    $previousErrorActionPreference = $ErrorActionPreference
    try {
        $ErrorActionPreference = 'Continue'
        $output = & $tool verify /pa /all /v /tw $Path 2>&1
        $exitCode = $LASTEXITCODE
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
    foreach ($line in $output) {
        Write-Host $line
    }
    if ($exitCode -ne 0) {
        throw "signtool verification failed with exit code $exitCode"
    }
}

function Get-CertificateSha256 {
    param(
        [Parameter(Mandatory = $true)]
        $Certificate
    )

    $algorithm = [System.Security.Cryptography.HashAlgorithmName]::SHA256
    $value = $Certificate.GetCertHashString($algorithm)
    return ConvertTo-NormalizedCertificateSha256 -Value $value
}

function Assert-ReleaseAuthenticodeSignature {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,

        [Parameter(Mandatory = $true)]
        [string] $ExpectedSignerSubject,

        [Parameter(Mandatory = $true)]
        [string] $ExpectedSignerSha256,

        [scriptblock] $SignatureProvider,

        [scriptblock] $SignToolVerifier
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Signed release executable was not found: $Path"
    }
    $resolvedPath = (Resolve-Path -LiteralPath $Path).ProviderPath
    $expectedSha256 = ConvertTo-NormalizedCertificateSha256 -Value $ExpectedSignerSha256

    $signature = if ($null -eq $SignatureProvider) {
        Get-AuthenticodeSignature -LiteralPath $resolvedPath -ErrorAction Stop
    } else {
        & $SignatureProvider $resolvedPath
    }
    if ($null -eq $signature) {
        throw 'Get-AuthenticodeSignature returned no result'
    }
    if ($signature.Status.ToString() -cne 'Valid') {
        throw "Authenticode status is $($signature.Status): $($signature.StatusMessage)"
    }
    if ($null -eq $signature.SignerCertificate) {
        throw 'The Authenticode signature has no signer certificate'
    }
    if ($null -eq $signature.TimeStamperCertificate) {
        throw 'The Authenticode signature has no timestamp countersignature'
    }
    if ($signature.SignerCertificate.Subject -cne $ExpectedSignerSubject) {
        throw "Unexpected signer subject: $($signature.SignerCertificate.Subject)"
    }

    $actualSha256 = Get-CertificateSha256 -Certificate $signature.SignerCertificate
    if ($actualSha256 -cne $expectedSha256) {
        throw "Unexpected signer certificate SHA-256: $actualSha256"
    }

    if ($null -eq $SignToolVerifier) {
        Invoke-SignToolVerification -Path $resolvedPath
    } else {
        $verificationOutput = & $SignToolVerifier $resolvedPath
        foreach ($line in @($verificationOutput)) {
            if ($null -ne $line) {
                Write-Host $line
            }
        }
    }

    Write-Host "Authenticode signature verified for $resolvedPath"
    Write-Host "Signer: $($signature.SignerCertificate.Subject)"
    Write-Host "Signer certificate SHA-256: $actualSha256"
    Write-Host "Timestamp signer: $($signature.TimeStamperCertificate.Subject)"
}

function Write-ReleaseChecksum {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Path,

        [Parameter(Mandatory = $true)]
        [string] $OutputPath
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Release asset was not found: $Path"
    }
    $resolvedPath = (Resolve-Path -LiteralPath $Path).ProviderPath
    $hash = (Get-FileHash -LiteralPath $resolvedPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $fileName = [IO.Path]::GetFileName($resolvedPath)
    [IO.File]::WriteAllText(
        [IO.Path]::GetFullPath($OutputPath),
        "$hash  $fileName`r`n",
        [Text.Encoding]::ASCII
    )
}

function Assert-SafeAssetName {
    param(
        [Parameter(Mandatory = $true)]
        [string] $AssetName
    )

    if ([string]::IsNullOrWhiteSpace($AssetName) -or
        [IO.Path]::GetFileName($AssetName) -cne $AssetName -or
        [IO.Path]::GetExtension($AssetName) -cne '.exe') {
        throw "Release asset name must be one .exe file name: $AssetName"
    }
}

function Complete-SignedReleaseAsset {
    param(
        [Parameter(Mandatory = $true)]
        [string] $SignedPath,

        [Parameter(Mandatory = $true)]
        [string] $OutputDirectory,

        [Parameter(Mandatory = $true)]
        [string] $AssetName,

        [Parameter(Mandatory = $true)]
        [string] $ExpectedSignerSubject,

        [Parameter(Mandatory = $true)]
        [string] $ExpectedSignerSha256,

        [scriptblock] $SignatureProvider,

        [scriptblock] $SignToolVerifier
    )

    Assert-SafeAssetName -AssetName $AssetName
    if (-not (Test-Path -LiteralPath $SignedPath -PathType Leaf)) {
        throw "Signed release executable was not found: $SignedPath"
    }

    $null = New-Item -ItemType Directory -Path $OutputDirectory -Force
    $destination = Join-Path $OutputDirectory $AssetName
    $checksum = "$destination.sha256"
    try {
        Copy-Item -LiteralPath $SignedPath -Destination $destination -Force
        Assert-ReleaseAuthenticodeSignature `
            -Path $destination `
            -ExpectedSignerSubject $ExpectedSignerSubject `
            -ExpectedSignerSha256 $ExpectedSignerSha256 `
            -SignatureProvider $SignatureProvider `
            -SignToolVerifier $SignToolVerifier
        Write-ReleaseChecksum -Path $destination -OutputPath $checksum
    } catch {
        Remove-Item -LiteralPath $destination -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $checksum -Force -ErrorAction SilentlyContinue
        throw
    }
}

function Complete-UnsignedReleaseAsset {
    param(
        [Parameter(Mandatory = $true)]
        [string] $UnsignedPath,

        [Parameter(Mandatory = $true)]
        [string] $OutputDirectory,

        [Parameter(Mandatory = $true)]
        [string] $AssetName
    )

    Assert-SafeAssetName -AssetName $AssetName
    if (-not (Test-Path -LiteralPath $UnsignedPath -PathType Leaf)) {
        throw "Unsigned release executable was not found: $UnsignedPath"
    }
    $null = New-Item -ItemType Directory -Path $OutputDirectory -Force
    $destination = Join-Path $OutputDirectory $AssetName
    Copy-Item -LiteralPath $UnsignedPath -Destination $destination -Force
    Write-ReleaseChecksum -Path $destination -OutputPath "$destination.sha256"
}

function Assert-ReleaseAssetBundle {
    param(
        [Parameter(Mandatory = $true)]
        [string] $Directory,

        [Parameter(Mandatory = $true)]
        [string] $AssetName
    )

    Assert-SafeAssetName -AssetName $AssetName
    if (-not (Test-Path -LiteralPath $Directory -PathType Container)) {
        throw "Release asset directory was not found: $Directory"
    }
    $entries = @(Get-ChildItem -LiteralPath $Directory -Force)
    $expectedNames = @($AssetName, "$AssetName.sha256") | Sort-Object
    $actualNames = @($entries | ForEach-Object { $_.Name } | Sort-Object)
    if ($entries.Count -ne 2 -or (Compare-Object $expectedNames $actualNames)) {
        throw "Release bundle must contain exactly $AssetName and $AssetName.sha256"
    }

    $assetPath = Join-Path $Directory $AssetName
    $checksumPath = "$assetPath.sha256"
    if (-not (Test-Path -LiteralPath $assetPath -PathType Leaf) -or
        -not (Test-Path -LiteralPath $checksumPath -PathType Leaf)) {
        throw 'Release bundle entries must both be regular files'
    }

    $content = [IO.File]::ReadAllText((Resolve-Path -LiteralPath $checksumPath).ProviderPath)
    $match = [regex]::Match(
        $content,
        '^(?<hash>[0-9a-f]{64})  (?<name>[^\r\n]+)(?:\r?\n)?\z'
    )
    if (-not $match.Success -or $match.Groups['name'].Value -cne $AssetName) {
        throw 'Release checksum file has an invalid format or asset name'
    }
    $actualHash = (Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualHash -cne $match.Groups['hash'].Value) {
        throw "Release checksum mismatch for $AssetName"
    }
}

Export-ModuleMember -Function @(
    'Assert-ReleaseAssetBundle',
    'Assert-ReleaseAuthenticodeSignature',
    'Assert-SignPathConfiguration',
    'Assert-SignPathToggle',
    'Complete-SignedReleaseAsset',
    'Complete-UnsignedReleaseAsset',
    'Find-SignTool',
    'Invoke-SignToolVerification',
    'Write-ReleaseChecksum'
)
