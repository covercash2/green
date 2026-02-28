watch . --glob=**/*.rs --debounce 5sec {|op, path, new_path|
  print $"File ($op): ($path) -- ($new_path)"
  let args = [
    cargo run
    "--"
    "--config-path" "config.dev.toml"
  ]

  print ($args | str join " ")

  run-external $args
}
