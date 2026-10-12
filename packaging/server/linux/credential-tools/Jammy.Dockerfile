FROM ubuntu@sha256:5ec03bb3441e8b0bf3b4f9cd4629a1ae763010dc3035bb8da3ae6cf026486401

# A build container, not a service manager or native-deployment evidence host.
RUN sed -n 's/^deb /deb-src /p' /etc/apt/sources.list >> /etc/apt/sources.list && \
    apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        build-essential=12.9ubuntu3 \
        pkg-config=0.29.2-1ubuntu3 \
        python3=3.10.6-1~22.04.1 \
        python3-pip=22.0.2+dfsg-1ubuntu0.7 \
        python3-jinja2=3.0.3-1ubuntu0.4 \
        ninja-build=1.10.1-1 \
        gperf=3.1-1build1 \
        libssl-dev=3.0.2-0ubuntu1.30 \
        libmount-dev=2.37.2-4ubuntu3.6 \
        libcap-dev=1:2.44-1ubuntu0.22.04.3 \
        libtss2-dev=3.2.0-1ubuntu1.1 \
        patchelf=0.14.3-1 && \
    printf '%s\n' 'meson==1.3.2 --hash=sha256:0ba4a71fbc060c44721c7b674807598c5af9ea51335073cae7a3e9a95b375c89' > /tmp/meson-requirements.txt && \
    python3 -I -m pip install --no-cache-dir --only-binary=:all: --require-hashes -r /tmp/meson-requirements.txt && \
    mkdir /opt/credential-sources && \
    for spec in \
        openssl=3.0.2-0ubuntu1.30 \
        util-linux=2.37.2-4ubuntu3.6 \
        libcap2=1:2.44-1ubuntu0.22.04.3 \
        tpm2-tss=3.2.0-1ubuntu1.1 \
        libselinux=3.3-1build2 \
        pcre2=10.39-3ubuntu0.1; do \
        name=${spec%%=*}; mkdir "/opt/credential-sources/$name"; \
        (cd "/opt/credential-sources/$name" && apt-get source --download-only "$spec") || exit 1; \
    done && \
    dpkg-query -W -f='${binary:Package}\t${Version}\t${source:Package}\t${source:Version}\n' > /opt/credential-sources/packages.tsv && \
    rm -rf /var/lib/apt/lists/* /var/cache/apt/archives/*

WORKDIR /driver
