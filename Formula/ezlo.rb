class Ezlo < Formula
  desc "Talking hat for your terminal: spec-driven zsh autocomplete & recipe launcher"
  homepage "https://github.com/denisidoro/ezlo"
  url "https://github.com/denisidoro/ezlo/archive/refs/tags/v0.1.0.tar.gz"
  sha256 "045288415f08d1541be5cb104f2d55578f136f74dbd1e9d097f920dd1f91139d"
  license "Apache-2.0"
  head "https://github.com/denisidoro/ezlo.git", branch: "main"

  depends_on "rust" => :build

  def install
    system "cargo", "install", *std_cargo_args
  end

  def caveats
    <<~EOS
      To enable ezlo, add this to your ~/.zshrc and open a new shell:
        eval "$(ezlo init zsh)"
    EOS
  end

  test do
    assert_match "ezlo", shell_output("#{bin}/ezlo init zsh")
  end
end
