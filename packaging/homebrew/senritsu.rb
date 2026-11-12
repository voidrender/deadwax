class Senritsu < Formula
  desc "Terminal music player for Navidrome and other Subsonic servers"
  homepage "https://github.com/voidrender/senritsu"
  url "https://github.com/voidrender/senritsu/archive/refs/tags/v0.1.0.tar.gz"
  # Replace with the release tarball's checksum: shasum -a 256 v0.1.0.tar.gz
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"
  license any_of: ["MIT", "Apache-2.0"]
  head "https://github.com/voidrender/senritsu.git", branch: "main"

  depends_on "rust" => :build
  depends_on "mpv"

  def install
    system "cargo", "install", *std_cargo_args

    return unless OS.mac?

    # The notification helper. Senritsu finds it in libexec next to its
    # own executable.
    system "scripts/install-macos-notifier.sh", "--build-only"
    libexec.install "target/notifier/SenritsuNotifier.app"
  end

  def caveats
    return unless OS.mac?

    <<~EOS
      The first track-change notification makes macOS ask whether
      SenritsuNotifier may show notifications. Allow it to see album art
      notifications.
    EOS
  end

  test do
    assert_match "senritsu #{version}", shell_output("#{bin}/senritsu --version")
    return unless OS.mac?

    assert_path_exists libexec/"SenritsuNotifier.app/Contents/MacOS/SenritsuNotifier"
    system "codesign", "--verify", libexec/"SenritsuNotifier.app"
  end
end
