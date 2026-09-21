param([Parameter(Mandatory = $true)][string]$Path)
$ErrorActionPreference = 'Stop'
$signature = Get-AuthenticodeSignature -LiteralPath $Path
if ($signature.Status -ne 'Valid') {
    throw "Invalid Authenticode signature: $($signature.Status): $($signature.StatusMessage)"
}
if ($null -eq $signature.SignerCertificate -or
    $signature.SignerCertificate.GetNameInfo([System.Security.Cryptography.X509Certificates.X509NameType]::SimpleName, $false) -cne 'Thinkery AG') {
    throw 'Unexpected Authenticode publisher; expected Thinkery AG'
}
if ($null -eq $signature.TimeStamperCertificate) {
    throw 'A trusted timestamp is required for short-lived Azure signing certificates'
}
$signature | Select-Object Status, Path,
    @{Name = 'Publisher'; Expression = { $_.SignerCertificate.Subject }},
    @{Name = 'TimestampAuthority'; Expression = { $_.TimeStamperCertificate.Subject }} | Format-List
Get-FileHash -LiteralPath $Path -Algorithm SHA256
