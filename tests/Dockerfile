# Test environment for the mc packages.
#
# Built from a stock Debian image on purpose: the suites have to be reproducible
# on a machine that has never run them, so this must not depend on any
# locally-tagged image. Debian 13 because that is what the packages target.
#
# No compiler here — the binaries arrive as built .debs from Dockerfile.build.
FROM debian:13-slim

# Both JREs are baked in rather than left to `mc install` at run time: the apt
# install dominates the runtime of the install-type matrix, and mc picks 25 for
# Minecraft 26.x and 21 for 1.21.x, so a full matrix needs both.
#
#   ca-certificates  TLS roots for the real upstream APIs (install-types only)
#   sudo             the privilege guards re-run through it; not a package
#                    dependency, since mc degrades to a plain refusal without it
#   zip              building fixture .mrpack files
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends \
      ca-certificates sudo zip \
      openjdk-21-jre-headless openjdk-25-jre-headless \
 && rm -rf /var/lib/apt/lists/*

# systemd is not running in a container, so the maintainer scripts and the mc
# CLI need something to talk to. See the file itself for what it pretends.
COPY systemctl-stub /usr/local/bin/systemctl
RUN chmod 755 /usr/local/bin/systemctl

# The presence test mc and the maintainer scripts gate on. Created here so the
# container exercises the systemd-present path rather than silently skipping it.
RUN mkdir -p /run/systemd/system

WORKDIR /work
