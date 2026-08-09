# Test environment for mc-server / mc-rcon.
#
# Built from a stock Debian image on purpose: the suites have to be reproducible
# on a machine that has never run them, so this must not depend on any
# locally-tagged image.
#
# Debian 13 because that is what the packages target, and because bash there is
# 5.x. An older bash — 3.x is still shipped as /bin/bash on some systems —
# differs enough that results from it are not evidence about this target (see
# the testing skill).
FROM debian:13-slim

# Both JREs are baked in rather than left to `ensure_java` at run time: the apt
# install dominates the runtime of the install-type matrix, and mc picks 25 for
# Minecraft 26.x and 21 for 1.21.x, so a full matrix needs both.
#
#   dpkg-deb (dpkg)  scripts/build.sh
#   gcc/make         mc-rcon's src/
#   libc6-dev        gcc alone has no headers — without it rcon.c fails at
#                    `#include <arpa/inet.h>`, mc-rcon silently never builds,
#                    and the failure only surfaces much later as a missing .deb
#   curl jq rsync    mc-server runtime Depends
#   unzip            .mrpack support     zip: building fixture packs
#   shellcheck       the lint suite
RUN apt-get update -qq \
 && apt-get install -y -qq --no-install-recommends \
      ca-certificates curl jq rsync unzip zip \
      gcc make libc6-dev \
      openjdk-21-jre-headless openjdk-25-jre-headless \
      shellcheck \
 && rm -rf /var/lib/apt/lists/*

# systemd is not running in a container, so the maintainer scripts and the mc
# CLI need something to talk to. See the file itself for what it pretends.
COPY systemctl-stub /usr/local/bin/systemctl
RUN chmod 755 /usr/local/bin/systemctl

WORKDIR /work
