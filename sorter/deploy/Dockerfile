# Build from the repository root so the two Worker modules the sorter shares
# come along: docker build -f sorter/deploy/Dockerfile .
FROM oven/bun:1.4-alpine
WORKDIR /app/sorter
COPY cloudflare/sona-companion/src/crypto.ts cloudflare/sona-companion/src/encoding.ts /app/cloudflare/sona-companion/src/
COPY sorter/package.json sorter/bun.lock ./
RUN bun install --frozen-lockfile --production
COPY sorter/src ./src
ENV SONA_SORTER_STATE=/state
VOLUME ["/state"]
ENTRYPOINT ["bun", "src/main.ts"]
CMD ["run"]
