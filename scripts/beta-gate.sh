#!/bin/sh
set -eu
make beta-local
make restore-attestation
