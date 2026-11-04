# The template of the Homebrew formula. packaging/render.sh puts in the
# version and the sums from SHA256SUMS. The homebrew job of
# .github/workflows/release.yml pushes the result to ember-luau/homebrew-tap.
class Embr < Formula
  desc "Package manager for Luau: publish, discover, and install packages"
  homepage "https://emberpm.dev"
  # Homebrew reads a version from the url when the formula gives none, and
  # finds 86.64 in "x86_64". The test then fails on `embr --version`.
  version "@VERSION@"
  license "MIT"

  on_macos do
    on_arm do
      url "https://github.com/ember-luau/ember/releases/download/v@VERSION@/embr-macos-aarch64.zip"
      sha256 "@SHA256_MACOS_AARCH64@"
    end
    on_intel do
      url "https://github.com/ember-luau/ember/releases/download/v@VERSION@/embr-macos-x86_64.zip"
      sha256 "@SHA256_MACOS_X86_64@"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/ember-luau/ember/releases/download/v@VERSION@/embr-linux-x86_64.zip"
      sha256 "@SHA256_LINUX_X86_64@"
    end
  end

  def install
    bin.install "embr"
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/embr --version")
  end
end
