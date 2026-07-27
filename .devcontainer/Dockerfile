FROM mcr.microsoft.com/playwright:v1.57.0-noble

ARG DIOXUS_VERSION=0.7.9

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        cmake \
        curl \
        git \
        gnupg \
        jq \
        libssl-dev \
        make \
        pkg-config \
        postgresql-client \
        python3 \
        python3-pip \
        ripgrep \
        shellcheck \
        sudo \
        tmux \
    && install -m 0755 -d /etc/apt/keyrings \
    && curl -fsSL https://download.docker.com/linux/ubuntu/gpg \
        -o /etc/apt/keyrings/docker.asc \
    && chmod a+r /etc/apt/keyrings/docker.asc \
    && . /etc/os-release \
    && echo \
        "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/ubuntu ${UBUNTU_CODENAME} stable" \
        > /etc/apt/sources.list.d/docker.list \
    && apt-get update \
    && apt-get install --yes --no-install-recommends \
        docker-ce-cli \
        docker-compose-plugin \
    && python3 -m pip install --break-system-packages --no-cache-dir pre-commit \
    && rm -rf /var/lib/apt/lists/*

RUN if id -u pwuser >/dev/null 2>&1; then \
        groupmod --new-name vscode pwuser \
        && usermod --login vscode --home /home/vscode --move-home \
            --gid vscode --shell /bin/bash pwuser; \
    else \
        groupadd --gid 1000 vscode \
        && useradd --uid 1000 --gid vscode --create-home --shell /bin/bash vscode; \
    fi \
    && echo "vscode ALL=(root) NOPASSWD:ALL" > /etc/sudoers.d/vscode \
    && chmod 0440 /etc/sudoers.d/vscode

RUN npm install --global "@openai/codex@latest"

USER vscode

ENV CARGO_HOME=/home/vscode/.cargo
ENV PATH=/home/vscode/.cargo/bin:${PATH}
ENV RUSTUP_HOME=/home/vscode/.rustup

COPY --chown=vscode:vscode rust-toolchain.toml /tmp/lumi-toolchain/rust-toolchain.toml
WORKDIR /tmp/lumi-toolchain

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --no-modify-path --profile minimal \
            --default-toolchain none \
    && rustup toolchain install \
    && cargo install dioxus-cli --version "${DIOXUS_VERSION}" --locked

WORKDIR /workspaces/lumi

CMD ["sleep", "infinity"]
