#! /usr/bin/env bash

RUSTFLAGS=-g cargo build --bin cpu --release --features profile_cpu_no_std

xcrun xctrace record \
  --template "Time Profiler" \
  --output profile.trace \
  --launch ./target/release/cpu 