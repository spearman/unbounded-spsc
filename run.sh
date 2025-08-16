#!/usr/bin/env bash

set -e
set -x

cargo run --example "$@"

exit 0
