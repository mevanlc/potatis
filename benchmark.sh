#!/usr/bin/env bash
cargo test -p nes --test benchmark -- --test-threads=1 --ignored --nocapture