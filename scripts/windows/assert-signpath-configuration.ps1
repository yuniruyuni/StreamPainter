$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

Import-Module (Join-Path $PSScriptRoot 'release-signing-lib.psm1') -Force

$apiTokenPresent = $env:SIGNPATH_API_TOKEN_PRESENT -ceq 'true'
Assert-SignPathConfiguration `
    -OrganizationId ([string] $env:SIGNPATH_ORGANIZATION_ID) `
    -ProjectSlug ([string] $env:SIGNPATH_PROJECT_SLUG) `
    -SigningPolicySlug ([string] $env:SIGNPATH_SIGNING_POLICY_SLUG) `
    -ArtifactConfigurationSlug ([string] $env:SIGNPATH_ARTIFACT_CONFIGURATION_SLUG) `
    -ExpectedSignerSubject ([string] $env:SIGNPATH_EXPECTED_SIGNER_SUBJECT) `
    -ExpectedSignerSha256 ([string] $env:SIGNPATH_EXPECTED_SIGNER_SHA256) `
    -ApiTokenPresent $apiTokenPresent

Write-Host 'SignPath identifiers, signer identity, and submitter token presence are configured.'
