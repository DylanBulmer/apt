FROM nginx:alpine

ARG VERSION
ARG RELEASE_TIME
ARG GIT_REPO
ARG GIT_COMMIT_SHA
ARG GIT_COMMIT_TIME
ARG GIT_REF

LABEL org.opencontainers.image.title="apt.bulmer.dev" \
      org.opencontainers.image.source="https://github.com/${GIT_REPO}" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.created="${RELEASE_TIME}" \
      org.opencontainers.image.revision="${GIT_COMMIT_SHA}"

COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY repo/dists/ /usr/share/nginx/html/dists/
COPY repo/pool/  /usr/share/nginx/html/pool/

# GPG public key for clients to verify the repo
COPY repo/bulmer.asc /usr/share/nginx/html/bulmer.asc
