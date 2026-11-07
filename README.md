<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/logo/spaceterm-icon-dark.svg">
    <img src="assets/brand/logo/spaceterm-icon.svg" width="128" height="128" alt="">
  </picture>
</p>

<h1 align="center">SpaceTerm</h1>

<p align="center">A fast native terminal multiplexer for macOS and Linux. Quit the app, and your terminals keep running.</p>

<p align="center">
  <a href="https://github.com/sadiksaifi/SpaceTerm/releases/latest"><img alt="Download for macOS" src="https://img.shields.io/badge/Download-macOS-10B981?style=for-the-badge&labelColor=1F2937&logo=apple&logoColor=white"></a>&emsp;<a href="https://github.com/sadiksaifi/SpaceTerm/releases/latest"><img alt="Download for Linux" src="https://img.shields.io/badge/Download-Linux-10B981?style=for-the-badge&labelColor=1F2937&logo=linux&logoColor=white"></a>
</p>

<p align="center">
  <img src="https://github.com/user-attachments/assets/5e1d29d7-0450-4984-b485-3c9f45fef473" width="90%" alt="SpaceTerm Screenshot">
</p>

## Highlights

- **Persistent terminals**: SpaceTerm Core runs your terminals in the background, separate from the app.
  - Quit the app, and they keep running.
  - Lose Wi-Fi, and they are still there when you reconnect.
  - Open SpaceTerm on another device, and you see the same terminals.
- **Remote Machines**: Machines far away feel local.
  - QUIC with TLS 1.3 keeps connections fast and encrypted.
  - Messages are encoded with postcard, a compact binary format.
  - Predictive Echo shows what you type before the remote Machine answers.
  - You add a Machine over SSH, an Invitation Link or a QR code.
- **Multiplexer**: Workspaces, Tabs and Panes keep your work organized.
  - Workspaces give each project its own space.
  - Tabs hold separate tasks inside a Workspace.
  - Panes split a Tab as many ways as you like, and you can zoom into one when you need to focus.
- **Keyboard control**: Everything works from the keyboard, and you can change any key binding in Settings.
- **Handoff**: Choose Take Over on another device to continue a live Pane right where you left it.
- **Kitty graphics protocol**: We support the Kitty graphics protocol for both local and remote Machines.
- **Accessibility**: We support VoiceOver on macOS and Orca on Linux.
- **Git worktrees**: We support Git worktrees right inside the app.
- **CLI**: `spaceterm` ships with SpaceTerm Core and can do almost everything the app can. Run `spaceterm --help` to see what it offers.

## Install

SpaceTerm runs on Apple silicon Macs with macOS 26 or later, and on x86_64 Linux with glibc 2.35 or newer.

- On a Mac, download the app from the [latest release](https://github.com/sadiksaifi/SpaceTerm/releases/latest), or install it with Homebrew:

  ```sh
  brew install --cask sadiksaifi/tap/spaceterm
  ```

- On Linux, download the `.tar.gz` from the [latest release](https://github.com/sadiksaifi/SpaceTerm/releases/latest), or install it with curl:

  ```sh
  curl -fsSL https://github.com/sadiksaifi/SpaceTerm/releases/latest/download/install.sh | sh -s -- --desktop
  ```

## Architecture

SpaceTerm has two parts:

- **SpaceTerm Core**: The service that runs in the background on each Machine.
  - It owns that Machine's Workspaces, Tabs, Panes and terminals.
  - It keeps them running while no client is connected.
  - It includes `spaceterm`, the CLI that controls Core from your shell.
- **SpaceTerm Client**: Desktop app that connects to your local and remote Machines and manages them.

How they connect:

- On the same Machine, a client talks to Core over a Unix domain socket that only your account can open.
- A remote Machine is reached over QUIC on UDP port 9231, so that port must be open.
- Add Machine in Settings > Machines sets up a new Machine. It uses SSH only for that first setup.

## More

- **Clipboard**: Programs on local and remote Machines can use your clipboard. See [terminal clipboard setup](docs/terminal-clipboard.md).
- **Permissions**: A program in a local Pane can ask SpaceTerm to set up macOS permissions. See [Permission Requests](docs/permission-request.md).

## License

SpaceTerm is open source under the [GNU GPL v3](LICENSE).
