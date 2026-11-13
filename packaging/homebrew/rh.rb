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
  version "1.0.0"
  license "MIT"

  livecheck do
    url :stable
    strategy :github_latest
  end

  depends_on "cloudflared"

  on_macos do
    on_intel do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v1.0.0/rh-v1.0.0-x86_64-apple-darwin.zip"
      sha256 "c6bcba1359cc7c0bba9b4d818193cce6f6bc1c7806f33a7a32203765b065960e"
    end
    on_arm do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v1.0.0/rh-v1.0.0-aarch64-apple-darwin.zip"
      sha256 "700c75dd5871c4b06e912f56170284ccb6f803f6e923fb18eb8a7dc394e4c126"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v1.0.0/rh-v1.0.0-x86_64-unknown-linux-gnu.zip"
      sha256 "45ec2ddfc8f99cb99fb21befb6f735e06c417b05ed2716c9811dc8b1ec00cea3"
    end
    on_arm do
      url "https://github.com/DavidMANZI-093/rabbit-hole/releases/download/v1.0.0/rh-v1.0.0-aarch64-unknown-linux-gnu.zip"
      sha256 "f3e50d3a2072aca17b7c093683d8d205baf5527c81ef2b849921341d6e57e45d"
    end
  end

  def install
    bin.install "rh"
  end

  test do
    assert_match "rabbit-hole", shell_output("#{bin}/rh --help")
  end
end
