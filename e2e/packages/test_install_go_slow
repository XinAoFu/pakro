#!/usr/bin/env bash
set -euo pipefail

# End-to-end: go runtime with GOMODCACHE pointed at the mise store. A
# stdlib-only project (no network needed for modules) verifies the env var
# and that builds work through the store.

cat > mise.toml <<'EOF'
[tools]
go = "1.23.4"
EOF

cat > go.mod <<'EOF'
module example.com/e2e

go 1.23
EOF

cat > main.go <<'EOF'
package main

import "fmt"

func main() {
  fmt.Println("hello from mise store")
}
EOF

mise i

# GOMODCACHE points at the mise store
assert "mise x -- go env GOMODCACHE" "${MISE_DATA_DIR}/store/go/mod"

# builds work through the store
assert "mise x -- go run main.go" "hello from mise store"

# a second project shares the same cache
mkdir -p proj2
cat > proj2/mise.toml <<'EOF'
[tools]
go = "1.23.4"
EOF

cat > proj2/go.mod <<'EOF'
module example.com/e2e2

go 1.23
EOF

(
  cd proj2
  mise i
  assert "mise x -- go env GOMODCACHE" "${MISE_DATA_DIR}/store/go/mod"
)
