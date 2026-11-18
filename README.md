# Platform acceptance probe

This standalone GitHub Actions workflow measures the standard hosted runner
configuration for an isolated operating system test. It checks Linux KVM and
disk capacity, identifies the stock Windows host edition, and verifies that a
native arm64 macOS process can be denied access to a synthetic loopback
listener while its runner remains connected to GitHub.
