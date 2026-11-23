FROM node:22-slim AS dev
RUN npm install -g pnpm@10
WORKDIR /project/front

FROM dev AS prod
WORKDIR /app
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
RUN pnpm run build
ENV HOST=0.0.0.0 PORT=3000
EXPOSE 3000
CMD ["node", "build"]
