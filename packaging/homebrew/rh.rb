# INFO: Homebrew formula for rabbit-hole (rh).
#
# Install into a tap repo named `homebrew-rh` as Formula/rh.rb
# (`gh repo create homebrew-rh --public`), then `brew tap <you>/rh`.
# Per release: bump `version`, refresh the four URLs, fill sha256
# (download the zips, `shasum -a 256`). Future CI can generate this file.
#
# NOTE: this formula downloads release assets unauthenticated, so it only
# works once the main repo is public. Until then it is complete but dormant.
class Rh < Formula
  desc "Send files and folders as-is, nothing stored in between, hash-verified per block."
  homepage "https://github.com/DavidMANZI-093/rabbit-hole"
  version "0.1.0"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on "cloudflared"

  on_macos do
    on_intel do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v0.1.0/rh-v0.1.0-x86_64-apple-darwin.zip"
      sha256 "5d84cad79d0985fb9d04c13e27be9c6d2748971b58fbdac66422061f28ecdc51"
    end
    on_arm do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v0.1.0/rh-v0.1.0-aarch64-apple-darwin.zip"
      sha256 "8daac640fc6a48c6c856dbb3446484bf37c35042663f909fc29dac5fca2540ca"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v0.1.0/rh-v0.1.0-x86_64-unknown-linux-gnu.zip"
      sha256 "6c851d386daf8833e231f9666520984347e81f469ebd9eb8be7b76f50e712375"
    end
    on_arm do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v0.1.0/rh-v0.1.0-aarch64-unknown-linux-gnu.zip"
      sha256 "f50ac931261a8029db20f25ace80d63609977ff58291105fc1aef4a6c4cf8cba"
    end
  end

  def install
    bin.install "rh"
  end

  test do
    assert_match "rabbit-hole", shell_output("#{bin}/rh --help")
  end
end
