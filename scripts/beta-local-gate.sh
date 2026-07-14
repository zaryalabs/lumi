#!/bin/sh
set -eu
make staging-config
make staging-smoke
make pg-t
make compatibility
make security
make performance
make restore-attestation-test
make c
make web-e2e
