# QMK on macOS

Keyjitsu can display layouts and collect heatmap data without installing QMK. Compiling your own firmware currently requires QMK and an ARM compiler. Installing the toolchain is a one-time step; subsequent builds reuse it. A managed toolchain installer is planned, but it isn't included in the current release.

The setup below is for a ZSA Voyager running firmware from the `firmware25` branch. Run commands in a normal Terminal window, one line at a time. Don't paste terminal prompts, Zellij decorations, or multiple commands as a single line.

## Install on a new Mac

1. Install the QMK command line tool with pipx, separate from Homebrew's QMK tap:

   ```sh
   brew install pipx git
   pipx install qmk
   pipx ensurepath
   ~/.local/bin/qmk --version
   ```

   Open a new Terminal if `qmk` isn't in `PATH` yet. The last command works even before restarting the shell. If pipx reports that QMK is already installed, use `pipx upgrade qmk` rather than reinstalling it.

2. Install an ARM cross-compiler from the standard Homebrew formula, without adding third-party taps:

   ```sh
   brew install arm-none-eabi-gcc
   arm-none-eabi-gcc --version
   ```

   This avoids the tap-trust errors reported by some Homebrew 6 installations. If you already have an ARM GCC toolchain that compiles the ZSA firmware, you can use that instead. A newly installed compiler should be tested with `qmk doctor` and a real build before considering setup complete.

3. Get the ZSA firmware source. This deliberately avoids `qmk setup` auto-installing other Homebrew formulae:

   ```sh
   git clone --branch firmware25 --recurse-submodules https://github.com/zsa/qmk_firmware.git "$HOME/qmk_firmware"
   ~/.local/bin/qmk config "user.qmk_home=$HOME/qmk_firmware"
   ```

   If `~/qmk_firmware` already exists, **don't clone over it**. Check its remote and branch first:

   ```sh
   git -C "$HOME/qmk_firmware" remote -v
   git -C "$HOME/qmk_firmware" branch --show-current
   ```

4. Run the checks:

   ```sh
   ~/.local/bin/qmk doctor
   scripts/check-qmk-macos.sh
   ```

   Run the second command from the Keyjitsu source repository, or use Keyjitsu → Settings → Firmware build to inspect what the app sees. A QMK doctor warning about AVR may not apply to a Voyager, which uses an ARM MCU; a missing ARM compiler does apply.

## Common failures

| Symptom | What to check |
| --- | --- |
| `zsh: command not found: qmk` | QMK CLI did not install or `~/.local/bin` is missing from the shell's `PATH`. Try `~/.local/bin/qmk --version`. |
| `brew install qmk/qmk/` fails | The package name is incomplete. This project recommends installing the CLI via pipx rather than depending on the QMK tap. |
| `Refusing to load formula ... from untrusted tap` | Don't add a third-party tap for this setup. Use the standard `brew install arm-none-eabi-gcc` formula instead. |
| `No available formula ... arm-none-eabi-gcc@8` | The older versioned formula isn't required for the basic setup. Use Homebrew core's `arm-none-eabi-gcc` and check that the firmware compiles. |
| `qmk setup: invalid option: -b` from brew | The shell received a broken command. `-b firmware25` is an option to `qmk setup`, not `brew tap`. |
| QMK works in Terminal but Keyjitsu says missing | On macOS, launchers may not inherit your shell `PATH`. Recent Keyjitsu versions also check `~/.local/bin` and Homebrew paths directly. Check the QMK CLI, ARM GCC and checkout separately in Settings. |
| Firmware checkout not detected | Check for `~/qmk_firmware/keyboards/zsa/voyager`. If your checkout is elsewhere, set its path in Keyjitsu Settings. |

The installer should not require administrator privileges for the app itself. Installing Homebrew packages and toolchains is a separate, user-controlled operation.

References: [QMK CLI](https://docs.qmk.fm/cli), [ZSA firmware25](https://github.com/zsa/qmk_firmware/tree/firmware25), [Homebrew tap trust](https://docs.brew.sh/Tap-Trust).
