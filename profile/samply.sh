#!/bin/sh
RUSTFLAGS=-g cargo build --bin cpu --release --features profile_cpu_no_std
samply record target/release/cpu
