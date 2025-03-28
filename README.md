# Cosmic Privacy Indicator

Privacy indicator for the COSMIC Desktop.

This applet detects Microphone and Camera usage, as well as Screen Sharing/Recording.

PipeWire is required for this applet to work.

![screenshot of the applet](./res/screenshot.png)

## Installation 

You can just grab the `.deb`, `.rpm` or tarball from the [releases](https://github.com/D-Brox/cosmic-ext-applet-privacy-indicator/releases/latest) page

## Building from source

Clone the repository:

```bash
git clone https://github.com/D-Brox/cosmic-ext-applet-privacy-indicator cosmic-ext-applet-privacy-indicator
cd cosmic-ext-applet-privacy-indicator
```

Install the build dependencies (or equivalent packages in non debian-based distros):

- libxkbcommon-dev
- libpipewire-0.3-dev 

Build and install the project:

```bash
just build-release
sudo just install
```

For alternative packaging methods, use the one of the following recipes:

- `deb`: run `just build-deb` and `sudo just install-deb`
- `rpm`: run `just build-rpm` and `sudo just install-rpm`

For vendoring, use `just vendor` and `just vendor-build`

## Contributing

A [justfile](./justfile) is included with common recipes used by other COSMIC projects:

- `just build-debug` compiles with debug profile
- `just run` builds and runs the application
- `just check` runs clippy on the project to check for linter warnings
- `just check-json` can be used by IDEs that support LSP

## License

Code is distributed with the [GPL-3.0-only license][./LICENSE]

