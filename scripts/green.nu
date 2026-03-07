# helper functions for green

const LOCAL_ADDRESS = "http://localhost:10000"

export def "green run" [
  config_path: path = "./config.dev.toml"
] {
  let args = [
    cargo run
    "--"
    "--config-path" $config_path
  ]

  print ($args | str join " ")

  run-external $args
}

export def "green index" [
  address: string@addresses = $LOCAL_ADDRESS
] {
  http get $address
}

export def "green ca" [
  address: string@addresses = $LOCAL_ADDRESS
] {
  http get $"($address)/ca"
}

def addresses [] {
  [$LOCAL_ADDRESS]
}
