# helper functions for green

const LOCAL_ADDRESS = "http://localhost:47336"

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
