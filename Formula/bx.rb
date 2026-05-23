class Bx < Formula
  desc "Run binaries from GitHub releases. npx for compiled tools, with MCP-server smarts"
  homepage "https://github.com/grahambrooks/bx"
  version "2026.5.23"
  license "MIT"

  # The sha256 lines below are kept in sync by .github/scripts/update_formula.py.
  # Do not remove the `# sha256:<platform>` sentinel comments — the updater
  # uses them to find each line and will fail the release if any go missing.

  # Apple Silicon only — Intel Macs are not a supported build target.
  # On an Intel Mac, Homebrew will report "no download URL"; that's intentional.
  on_macos do
    on_arm do
      url "https://github.com/grahambrooks/bx/releases/download/v#{version}/bx-#{version}-darwin-arm64.tar.gz"
      sha256 "0dd418a3049e1751a5cc51847e2abdac93ce341259a6eb8525e7c84cccbb72a3" # sha256:darwin_arm64
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/grahambrooks/bx/releases/download/v#{version}/bx-#{version}-linux-arm64.tar.gz"
      sha256 "97177d4476da2a71700b76d59f1bcf19c3ae5649753c19715c797a737e43fab1" # sha256:linux_arm64
    end
    on_intel do
      url "https://github.com/grahambrooks/bx/releases/download/v#{version}/bx-#{version}-linux-x64.tar.gz"
      sha256 "27ca18632ee9b5b05124eea3aed4f2baf71d9a8300ed54e2859994795ade313a" # sha256:linux_x64
    end
  end

  def install
    bin.install "bx"
  end

  test do
    assert_match(/bx/i, shell_output("#{bin}/bx --help"))
  end
end
