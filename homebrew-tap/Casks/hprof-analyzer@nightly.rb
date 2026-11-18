cask "hprof-analyzer@nightly" do
  version :latest
  sha256 :no_check

  url "https://github.com/parttimenerd/hprof-analyzer/releases/download/nightly/hprof-analyzer-aarch64-apple-darwin.tar.gz"
  name "hprof-analyzer"
  desc "Fast, low-memory Java HPROF heap-dump analyzer (nightly build)"
  homepage "https://github.com/parttimenerd/hprof-analyzer"

  depends_on arch: :arm64

  binary "hprof-analyzer-aarch64-apple-darwin/hprof-analyzer"

  caveats do
    "Uninstall the stable hprof-analyzer formula first if both are needed on the same box: `brew uninstall hprof-analyzer`"
  end
end
