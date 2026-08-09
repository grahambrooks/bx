class Bx < Formula
  desc "Run binaries from GitHub releases. npx for compiled tools, with MCP-server smarts"
  homepage "https://github.com/grahambrooks/bx"
  version "2026.8.9"
  license "MIT"

  # The sha256 lines below are kept in sync by .github/scripts/update_formula.py.
  # Do not remove the `# sha256:<platform>` sentinel comments — the updater
  # uses them to find each line and will fail the release if any go missing.

  # Apple Silicon only — Intel Macs are not a supported build target.
  # On an Intel Mac, Homebrew will report "no download URL"; that's intentional.
  on_macos do
    on_arm do
      url "https://github.com/grahambrooks/bx/releases/download/v#{version}/bx-#{version}-darwin-arm64.tar.gz"
      sha256 "83727af65d6c30969d6ff388d8da2297ac42761bd7fe03dc6118925360aa40d4" # sha256:darwin_arm64
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/grahambrooks/bx/releases/download/v#{version}/bx-#{version}-linux-arm64.tar.gz"
      sha256 "fbe126d125a2ba08e0f839759235b8ba6fa62b4ed456fee65f9dc561f2270cd8" # sha256:linux_arm64
    end
    on_intel do
      url "https://github.com/grahambrooks/bx/releases/download/v#{version}/bx-#{version}-linux-x64.tar.gz"
      sha256 "cd5f387e1f07547ec42da4ebcef0576e3bc1f6db1a94b4605f928d4f939fc5e5" # sha256:linux_x64
    end
  end

  def install
    bin.install "bx"
  end

  test do
    assert_match(/bx/i, shell_output("#{bin}/bx --help"))
  end
end
