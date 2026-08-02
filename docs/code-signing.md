# Code signing policy

## 現在の状態

2026-08-03時点では、SignPath Foundationの承認、SignPath organization/project/policy/
artifact configuration、証明書が未取得です。GitHubの`code-signing` environmentと関連する
secret/variablesもまだ存在せず、`SIGNPATH_ENABLED`は設定していません。このため現在の公式Releaseは
従来どおりunsignedです。署名済みであるとは表示しません。

Release workflowには本番値を含まないscaffoldだけを用意しています。`SIGNPATH_ENABLED`が未設定または
厳密に`false`なら従来のunsigned経路を使います。`true`へ切り替えた後は署名失敗時にunsignedへ
fallbackせず、Release公開を止めます。大文字の`TRUE`など不明な値もbuild時に拒否します。

承認・設定・検証をすべて終えた後、この節とREADMEを実際の証明書情報へ更新し、次のFoundation指定
表示を有効な事実として掲載します。署名経路のrelease pageにもworkflowが同じ表示を自動で追加し、
unsigned経路では署名されていないことを明記します。

> Free code signing provided by SignPath.io, certificate by SignPath Foundation

根拠となる現行一次情報は[SignPath Foundationの条件](https://signpath.org/terms.html)、
[SignPath GitHub trusted build integration](https://docs.signpath.io/trusted-build-systems/github)、
[SignPath project/policy設定](https://docs.signpath.io/projects)、
[Artifact Configuration](https://docs.signpath.io/artifact-configuration/)です。

## サービス選定

第一候補はSignPath FoundationのOpen Source Code Signingです。公開OSS向けの証明書、SignPathが管理する
秘密鍵（[HSM上で生成して外へ出さない方式](https://docs.signpath.io/managing-certificates)）、GitHub trusted
build/origin verification、requestごとのmanual approvalを組み合わせられるため、
PFXや証明書秘密鍵をGitHub Actionsへ保管せずにrelease provenanceを検証できます。この利点を優先し、
Foundationから承認されるまでは選定を確定扱いにしません。

Foundationが不承認、終了、または要件不適合の場合は、公開CAの有償code-signing certificateか、秘密鍵を
外部HSMで管理できる別のsigning serviceを再評価します。ローカルPFXをActions secretへ格納する方式は、
秘密鍵のexport・漏えい・rotation範囲が広がるため第一候補にしません。どの方式でも同じprotected tag、
manual approval、署名後検証、checksum順序、fail-closed公開条件を維持します。

Authenticode署名はpublisherと署名後の完全性を検証可能にしますが、Microsoft SmartScreenのreputationや
警告の即時解消を保証するものではありません。

## 適格性評価

StreamPainterはMITライセンスの公開repositoryで、既にWindows向けにreleaseされ、機能とbuild方法を
公開しています。配布exeはこのrepositoryのRust/TypeScript sourceとlockfileからGitHub-hosted runnerで
buildし、別projectのproprietary binaryを同梱しません。脆弱性の探索・防御回避を目的とする製品でも
ありません。そのためFoundation条件の基本部分には合致すると考えます。

ただし、適格性と証明書提供はSignPath Foundationだけが判断できます。次は承認前の未解決事項です。

- Foundationへ申請し、projectの評判・repository支配・製品内容について承認を得る。
- committers/reviewers/approversがGitHubとSignPathのMFAを有効にしていることを運用上確認する。
- SignPath上のroles、manual approval、certificate、trusted build/origin policyを確定する。
- 保護されたtagから発生するGitHub origin metadataをSignPathで実測し、許可するoriginをその経路だけに
  制限する。tag eventで使う値を推測してpolicyへ入力しない。
- GitHubのmain/tag ruleset、`code-signing` environment、immutable releasesを有効にする。
- tagをpushするactorとは別のGitHub environment reviewerを決める。現在は未定であり、self-review禁止の
  environmentを安全に運用できるreviewerが参加するまで署名を有効化しない。

Windows VERSIONINFOはbuild時にCargo versionから生成し、`ProductName=StreamPainter`、
`ProductVersion`/`FileVersion=<Cargo version>`、`OriginalFilename=stream-painter.exe`を設定します。
これはFoundationが要求するmetadata restrictionを設定可能にするためのものです。

## Team roles and privacy

承認申請時の初期role案は次のとおりです。SignPath上の実ユーザーと権限を作成した後に再確認します。

- Committer / reviewer: [@yuniruyuni](https://github.com/yuniruyuni)
- Signing approver: [@yuniruyuni](https://github.com/yuniruyuni)
- CI submitter: SignPathの`release-signing` policyへsubmitだけできる専用CI user
- GitHub `code-signing` environment reviewer: 未定（release tagをpushするactorとは別のtrusted user）

実行中のStreamPainterについてのprivacy statementは次のとおりです。

> This program will not transfer any information to other networked systems unless specifically
> requested by the user or the person installing or operating it.

StreamPainterはloopbackのBrowser Sourceと、利用者が設定したOBS WebSocketへだけ接続します。telemetryや
自動更新通信はありません。コード署名時にGitHub Actionsがrelease binaryをSignPathへ渡す処理は
maintainerのbuild pipelineであり、利用者の実行データを送信するものではありません。

portable版を削除する場合はアプリを終了してexeを削除します。設定・stampも消す場合は
`%APPDATA%\StreamPainter`、logも消す場合は`%LOCALAPPDATA%\StreamPainter`を削除し、Windows資格情報
マネージャーのGeneric credential `StreamPainter/OBS WebSocket`も削除します。

## 脅威モデルと制御

| 脅威 | 制御 |
| --- | --- |
| API tokenの漏えい | tokenはrepositoryへ置かず`code-signing` environment secretに保存する。専用CI userには指定project/policyへのsubmitだけを許可し、approve/configure権限を与えない。 |
| fork/PRからの秘密利用 | 署名jobはupstreamの厳密な`vX.Y.Z` tag pushだけで起動し、PR workflowから呼ばない。GitHubもfork PRへActions secretsを渡さない。 |
| 改変されたbranch/tagの署名 | release commitが`origin/main`に含まれること、tagとCargo versionが一致することを署名前に検証する。mainと`v*` tagをrulesetで保護し、tag作成者を限定する。 |
| CIやartifactのなりすまし | GitHub.com trusted build systemとorigin verificationを必須にし、署名前artifactをGitHub artifactとして保存してそのartifact IDをSignPath公式actionへ渡す。先行jobはGitHub-hosted runnerだけを使う。 |
| third-party Actionの差し替え | checkout、upload/download-artifact、SignPath actionをすべて完全commit SHAへpinする。SignPath action v2.2は`b9d91eadd323de506c0c81cf0c7fe7438f3360fd`へpinしている。 |
| 意図しないfileの署名 | artifact configurationは1つのZIP内の厳密なexe名、製品名、version、original filenameを制限し、そのPEだけへSHA-256 Authenticodeを付与する。 |
| 別証明書・timestampなし・壊れたchain | final exeでAuthenticode `Valid`、割り当て済みsubject、leaf certificate SHA-256、timestamp certificateの存在を検査し、さらに`signtool verify /pa /all /v /tw`を成功必須にする。 |
| 署名後fileの差し替え | 検証対象をfinal `dist`へcopyしてから署名を再検証し、その後にだけSHA-256を生成する。publish jobでもassetがexeとchecksumの2つだけでhashが一致することを再検証する。 |
| 署名拒否・timeout・provider障害 | `SIGNPATH_ENABLED=true`時はsign job成功だけがpublish条件であり、unsigned jobはskipされる。失敗、denied、timeout、出力欠落のいずれでもReleaseを作らない。 |
| 公開後のtag/asset置換 | `gh release create --verify-tag --fail-on-no-commits`を維持し、既存releaseを更新しない。GitHub immutable releasesも有効化し、修正版は新versionで公開する。 |

[GitHubはfork PRへActions secretを渡さない](https://docs.github.com/en/code-security/reference/secret-security/secret-types)
一方、同じrepositoryへmergeされた悪意あるworkflowはsecretを参照できます。そのためworkflow、build script、
artifact helper、CODEOWNERS自体をCODEOWNERS対象にし、main rulesetで第三者reviewを必須にすることが重要です。

## SignPath側の必須設定

値はFoundation承認後にSignPath UIから取得し、repositoryへhard-codeしません。

1. [Foundationへ申請](https://signpath.org/apply)し、承認を得る。
2. SignPath organizationへpredefined trusted build system `GitHub.com`を追加し、StreamPainter projectへ
   linkする。repository URLは`https://github.com/yuniruyuni/StreamPainter`へ固定する。
3. Open Source Code Signingのrelease policyでtrusted build system verificationとorigin verificationを
   必須にし、Foundation条件どおり各requestへmanual approvalを要求する。CI userはsubmitterだけ、
   maintainerはapproverとして分離する。
4. Protected `vX.Y.Z` tag workflowから得たoriginを確認し、そのrelease経路だけをpolicyで許可する。
   SignPathの一般例にある`main`を、tag eventでも同じ値になると仮定しない。
5. versioned artifact configurationを作り、そのslugを固定する。upload-artifactが作るZIPをrootにし、
   workflowから渡す必須parameter `version`を使って概ね次の制約を設定する。

```xml
<artifact-configuration xmlns="http://signpath.io/artifact-configuration/v1">
  <parameters>
    <parameter name="version" required="true" />
  </parameters>
  <zip-file>
    <pe-file path="StreamPainter-v${version}-windows-x64.exe"
             product-name="StreamPainter"
             product-version="${version}"
             file-version="${version}"
             original-filename="stream-painter.exe">
      <authenticode-sign hash-algorithm="sha256"
                         description="StreamPainter"
                         description-url="https://github.com/yuniruyuni/StreamPainter" />
    </pe-file>
  </zip-file>
</artifact-configuration>
```

実際のXMLはSignPath UIの現行schemaでvalidateし、Foundation担当者と確認します。default artifact
configurationへ暗黙依存せず、承認済みversionのslugをworkflow variableへ設定します。timestamp方式と
証明書はFoundation/policy側の割当を使い、SHA-256署名と信頼済みtimestampが最終検証を通ることを
test requestで確認します。

## GitHub側の必須設定と有効化順序

調査時点でrepository ruleset、environment、Actions variablesは未設定で、最新Releaseの
`immutable`もfalseでした。次をすべて完了するまでは`SIGNPATH_ENABLED`を作成しません。

1. main rulesetでPR reviewとworkflow/build scriptのCODEOWNER reviewを要求し、force pushを禁止する。
2. active tag rulesetを`v*`へ適用し、release maintainerだけをbypass actorとしてtagの作成を許可する。
   updateとdeleteも制限する。GitHubの[タグruleset](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-rulesets/available-rules-for-rulesets)を使う。
3. GitHubの[immutable releases](https://docs.github.com/en/code-security/concepts/supply-chain-security/immutable-releases)を
   有効にする。
4. `code-signing` environmentを作り、required reviewer、self-review禁止、admin bypass禁止、protected tag
   だけのdeployment ruleを設定する。GitHubによれば、approval前は
   [environment secretへjobがアクセスできません](https://docs.github.com/en/actions/reference/workflows-and-actions/deployments-and-environments)。
5. `code-signing` environment secretへ`SIGNPATH_API_TOKEN`、environment variablesへ次を設定する。

| variable | 値の取得元 |
| --- | --- |
| `SIGNPATH_ORGANIZATION_ID` | SignPath organization |
| `SIGNPATH_PROJECT_SLUG` | 承認済みStreamPainter project |
| `SIGNPATH_SIGNING_POLICY_SLUG` | manual approvalとorigin verification付きrelease policy |
| `SIGNPATH_ARTIFACT_CONFIGURATION_SLUG` | 上記versioned artifact configuration |
| `SIGNPATH_EXPECTED_SIGNER_SUBJECT` | Foundationから実際に割り当てられたleaf certificateの完全なSubject |
| `SIGNPATH_EXPECTED_SIGNER_SHA256` | 同じleaf certificate DERのSHA-256 fingerprint（64 hex） |

6. 公開証明書ではないtest policyでprotected tag相当のorigin、manual approval、署名fileの出力位置、
   subject/fingerprint、timestamp、chain、metadata制約を確認する。
7. Code signing policyの現在状態、README、利用者向けexpected publisher/fingerprintを実値へ更新するPRを
   reviewしてmainへmergeする。
8. 最後にrepository variable `SIGNPATH_ENABLED=true`を小文字で設定する。有効化後は署名失敗を回避する
   ためだけに`false`へ戻さない。同じtagを再利用せず、原因を修正して新versionを署名付きでreleaseする。
   意図的にunsigned配布へ戻す場合は、緊急時の例外としてpolicyとREADMEを更新するreview済みPRを先に
   mergeし、利用者へ明示する。

## Release処理の順序

```text
protected vX.Y.Z tag
  -> main ancestry / Cargo version / tests / Windows build
  -> unsigned GitHub artifact (1日保持、公開assetではない)
  -> code-signing environment approval
  -> SignPath trusted GitHub request + SignPath manual approval
  -> signed exe download
  -> Authenticode subject/fingerprint/timestamp/chain verification
  -> SHA-256生成
  -> final artifactのexact file set/hash再検証
  -> GitHub Release create
```

build/sign jobにRelease書き込み権限は与えず、最終publish jobだけを`contents: write`にしています。

## 利用者による確認

承認・有効化後のReleaseでは、まずrelease pageからlinkされたこのpolicyに掲載するexpected publisherと
certificate fingerprintを比較します。その上でWindows Explorerのexeのプロパティから「デジタル署名」を開き、
署名者、timestamp、証明書pathが正常であることを確認できます。次の2つのexpected値を、このpolicyの
対象Release tagに掲載された実値へ置き換えてから、Windows PowerShell 5.1で確認します。

```powershell
$file = ".\StreamPainter-vX.Y.Z-windows-x64.exe"
$expectedSignerSubject = "<このpolicyに掲載された完全なcertificate Subject>"
$expectedSignerSha256 = "<このpolicyに掲載された64桁のSHA-256 fingerprint>"

function ConvertTo-NormalizedSha256Fingerprint {
  param([Parameter(Mandatory = $true)][string] $Value)

  # Certificate UIなどで使われる空白、コロン、ハイフンだけを区切り文字として許可する。
  $normalized = ($Value -replace '[:\s-]', '').ToUpperInvariant()
  if ($normalized -cnotmatch '\A[0-9A-F]{64}\z') {
    throw "Expected signer SHA-256 fingerprint must contain exactly 64 hexadecimal digits"
  }
  return $normalized
}

$expectedSignerSha256 = ConvertTo-NormalizedSha256Fingerprint $expectedSignerSha256
$signature = Get-AuthenticodeSignature -LiteralPath $file
if ($signature.Status -ne "Valid" -or
    $null -eq $signature.SignerCertificate -or
    $null -eq $signature.TimeStamperCertificate) {
  throw "StreamPainter signature or timestamp is invalid"
}

# RawDataはleaf certificateのDER encoding。SHA256.Create()はWindows PowerShell 5.1互換。
$sha256 = [System.Security.Cryptography.SHA256]::Create()
try {
  $actualSignerSha256 = [System.BitConverter]::ToString(
    $sha256.ComputeHash($signature.SignerCertificate.RawData)
  ).Replace('-', '')
} finally {
  $sha256.Dispose()
}

$actualSignerSubject = $signature.SignerCertificate.Subject
[pscustomobject]@{
  Status = $signature.Status
  StatusMessage = $signature.StatusMessage
  SignerSubject = $actualSignerSubject
  SignerSha256 = $actualSignerSha256
  TimeStamperSubject = $signature.TimeStamperCertificate.Subject
} | Format-List

if ($actualSignerSubject -cne $expectedSignerSubject) {
  throw "Unexpected StreamPainter signer Subject: $actualSignerSubject"
}
if ($actualSignerSha256 -cne $expectedSignerSha256) {
  throw "Unexpected StreamPainter signer SHA-256 fingerprint: $actualSignerSha256"
}
```

Windows SDKがある場合はMicrosoft推奨のDefault Authenticode policyでchainも検証します。

```powershell
signtool.exe verify /pa /all /v /tw .\StreamPainter-vX.Y.Z-windows-x64.exe
```

Microsoftの[SignTool documentation](https://learn.microsoft.com/en-us/windows/win32/seccrypto/signtool)に
よれば、`verify`は発行CA、revocation、policyを検証し、`/pa`はDefault Authenticode policy、`/tw`は
timestamp欠落を警告します。workflowはこれに加えてtimestamp certificateの存在を明示的に必須にします。

最後に、署名検証済みの同じexeについて添付checksumを確認します。

```powershell
Get-FileHash -Algorithm SHA256 .\StreamPainter-vX.Y.Z-windows-x64.exe
Get-Content .\StreamPainter-vX.Y.Z-windows-x64.exe.sha256
```

Authenticodeは発行元と署名後の完全性を示しますが、無欠陥・無脆弱性を保証するものではありません。
