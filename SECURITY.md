# Security Policy

## Supported Versions

The library is in active development; only the latest version is supported.

## Reporting a Vulnerability

Please report **any** security issue you find in the library. You can do so by
[opening a new issue](https://github.com/ziv/bevytiles/issues/new) and marking it as a security issue. We will respond
to your report as soon as possible and work with you to resolve it.

## Security Best Practices

Tile downloads use HTTPS with certificate validation via `rustls`; unlike raytiles, this library provides **no option
to bypass TLS certificate validation**. If you point the provider URL templates at your own tile server, use `https://`
endpoints with valid certificates.

Downloaded tiles are written to the on-disk cache (`NetworkConfig::cache_dir`) exactly as received; decoded bytes are
size-capped per response. Treat the cache directory as untrusted input if you share it between machines.
