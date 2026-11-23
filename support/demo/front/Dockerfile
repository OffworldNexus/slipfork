FROM node:22-slim AS dev
# `corepack enable` provides the pinned pnpm (from `packageManager`) instead of a global install.
RUN corepack enable
WORKDIR /project/front

# This demo has no runtime `dependencies`, so the runtime image needs neither `node_modules` nor
# devDependencies; `@sveltejs/adapter-node` bundles the server into `build/`.
FROM dev AS build
WORKDIR /app
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm run build

FROM node:22-slim AS prod
WORKDIR /app
ENV NODE_ENV=production HOST=0.0.0.0 PORT=3000
COPY --from=build --chown=1000:1000 /app/build ./build
COPY --from=build --chown=1000:1000 /app/package.json ./
EXPOSE 3000
CMD ["node", "build"]
