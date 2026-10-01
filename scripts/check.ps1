# One command for "is the tree healthy": format, lint, unit tests. -Live adds the opt-in
# live tests (they open the fixture window and briefly use the desktop; they send no input
# except one guarded Enter in the combo test). Never run -Live while you are using the PC.
param([switch]$Live)
$ErrorActionPreference = 'Stop'
function Step($name, [scriptblock]$cmd) {
  Write-Host "== $name"
  & $cmd
  if ($LASTEXITCODE -ne 0) { Write-Error "$name failed"; exit 1 }
}
Step 'format'  { cargo fmt --all -- --check }
Step 'clippy'  { cargo clippy --workspace --all-targets -- -D warnings }
Step 'tests'   { cargo test --workspace }
if ($Live) {
  Step 'live: fixture + mcp' { cargo test -p winwright-cli --test live_fixture --test mcp_stdio -- --ignored --test-threads=1 }
  Step 'live: confirm dialog' { cargo test -p winwright-overlay --test live_confirm -- --ignored }
}
Write-Host 'all checks passed'
