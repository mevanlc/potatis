#!/bin/sh
RUSTFLAGS=-g cargo run --bin memory --release
open 'https://nnethercote.github.io/dh_view/dh_view.html'
