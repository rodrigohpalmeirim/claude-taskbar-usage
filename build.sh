#!/usr/bin/env sh
# Cross-compiles the Windows binary from any OS with Go installed.
# VERSION=1.2.3 ./build.sh stamps the version into the exe (default: dev).
set -e
cd "$(dirname "$0")"
VERSION="${VERSION:-dev}"
WINRES_VERSION=""
case "$VERSION" in
  [0-9]*.[0-9]*.[0-9]*) WINRES_VERSION="--product-version $VERSION --file-version $VERSION" ;;
esac
# shellcheck disable=SC2086
go run github.com/tc-hib/go-winres@v0.3.3 make --in winres/winres.json --arch amd64 $WINRES_VERSION
GOOS=windows GOARCH=amd64 go build -trimpath -ldflags "-s -w -H=windowsgui -X main.version=$VERSION" -o dist/ClaudeTaskbarUsage.exe .
echo "built dist/ClaudeTaskbarUsage.exe ($VERSION)"
