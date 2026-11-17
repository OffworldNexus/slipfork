# OSPP v1

## What OSPP is

OSPP v1 is a protocol for obtaining the secrets and values a local world needs from the same place CI gets them: a team's cloud-in-a-box. Slipfork owns OSPP v1.

## What ships in `0.1.0`

`vars::`, `secrets::`, and `!!` resolve locally, and `!!` is generated once and persisted. Nothing in `0.1.0` requires a provider: the OSPP v1 client ships, but no provider implements the protocol yet.

## The protocol

The OSPP v1 wire format is not published.
A paired session would supply remote secrets and seed dumps.

## When you'll touch it

- `!!` in `snow.yml` today: the value is generated once and reused.
- `slipfork auth login <url>` pairs this machine with an OSPP v1 provider; no provider ships in `0.1.0`.
