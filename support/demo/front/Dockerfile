FROM node:22-slim AS dev
RUN npm install -g pnpm@10
WORKDIR /project/front

FROM dev AS prod
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm run build
