# list recipes by default
default:
  just --list

run:
  cargo run -- --ca-path /var/lib/mkcert/rootCA.pem

test:
  cargo test
