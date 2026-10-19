// Frozen reduced official metadata, read 2026-10-15.
// PBS: https://api.github.com/repos/astral-sh/python-build-standalone/releases/latest
// PyPy: https://downloads.python.org/pypy/versions.json
// SHA256: https://pypy.org/checksums.html
// Only modeled fields/matching macOS aarch64 archives retained.
// Rust literals keep fixtures inside the existing Cargo package include scope.

const PBS_FIXTURE: &str = r###"[
  {
    "tag_name": "20261013",
    "draft": false,
    "prerelease": false,
    "published_at": "2026-10-14T02:06:09Z",
    "assets": [
      {
        "name": "cpython-3.15.0+20261013-aarch64-apple-darwin-freethreaded-install_only.tar.gz",
        "browser_download_url": "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-3.15.0%2B20261013-aarch64-apple-darwin-freethreaded-install_only.tar.gz",
        "digest": "sha256:9b982a1644405461bb0c96efecff18538e259c3ac253056f25a423fdce8fc069"
      },
      {
        "name": "cpython-3.15.0+20261013-aarch64-apple-darwin-freethreaded-install_only_stripped.tar.gz",
        "browser_download_url": "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-3.15.0%2B20261013-aarch64-apple-darwin-freethreaded-install_only_stripped.tar.gz",
        "digest": "sha256:94e34d776cff596017d15fce1b2175d463628a765ddb37412a39aaba6d31bf67"
      },
      {
        "name": "cpython-3.15.0+20261013-aarch64-apple-darwin-install_only.tar.gz",
        "browser_download_url": "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-3.15.0%2B20261013-aarch64-apple-darwin-install_only.tar.gz",
        "digest": "sha256:f5d1d82e2f5660d4decae5d2b7470631850bcd7bd39595d9c3cca3c5ee54eae7"
      },
      {
        "name": "cpython-3.15.0+20261013-aarch64-apple-darwin-install_only_stripped.tar.gz",
        "browser_download_url": "https://github.com/astral-sh/python-build-standalone/releases/download/20261013/cpython-3.15.0%2B20261013-aarch64-apple-darwin-install_only_stripped.tar.gz",
        "digest": "sha256:b4ab2009803cef142e4ebf2ebef225f6875f83cc35caf4eefb437aa9b2b76783"
      }
    ]
  }
]
"###;

const PYPY_FIXTURE: &str = r###"[
  {
    "pypy_version": "8.0.1",
    "python_version": "3.12.15",
    "date": "2026-10-14",
    "stable": true,
    "files": [
      {
        "filename": "pypy3.12-v8.0.1-macos_arm64.tar.gz",
        "arch": "arm64",
        "platform": "darwin",
        "download_url": "https://downloads.python.org/pypy/pypy3.12-v8.0.1-macos_arm64.tar.gz"
      }
    ]
  }
]
"###;

const PYPY_CHECKSUMS: &str = r###"<!doctype html>
<html><body><h2>pypy3.12-v8.0.1 sha256:</h2>
<pre class="literal-block">c10baeae4a61063a1183c4ee4e8324765ba736a3980323654efb31e79f1514db  pypy3.12-v8.0.1-macos_arm64.tar.gz</pre></body></html>
"###;
