# Real sway on a headless output, with swayview capturing windows in each
# pixel format wlroots' OpenGL renderer can offer. Build from the repo root:
#
#     docker build -f e2e-test/Dockerfile -t swayview-e2e .
FROM archlinux:latest
RUN pacman -Syu --noconfirm --needed sway grim mesa ttf-dejavu rust gcc pkgconf libxkbcommon libcap python-numpy \
    && pacman -Scc --noconfirm
# Arch gives sway cap_sys_nice, which a container does not allow, so it would
# not even start; it needs no higher priority for a test.
RUN sway=$(readlink -f "$(command -v sway)") && if [ -n "$(getcap "$sway")" ]; then setcap -r "$sway"; fi
WORKDIR /src
COPY . .
RUN cargo build --release --locked --workspace \
    && install -m755 target/release/swayview target/release/fake-window /usr/local/bin/ \
    && gcc -shared -fPIC -O2 -Wall -o /usr/local/lib/readformat.so e2e-test/readformat.c -ldl -lm
ENTRYPOINT ["/src/e2e-test/run.sh"]
