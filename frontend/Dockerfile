# syntax=docker/dockerfile:1.7
FROM node:24-bookworm-slim AS build
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY tsconfig.json vite.config.ts index.html ./
COPY src ./src
COPY server ./server
COPY scripts ./scripts
COPY public ./public
RUN npm test && npm run build

FROM node:24-bookworm-slim
ENV NODE_ENV=production HOST=0.0.0.0 PORT=4330
WORKDIR /app
COPY --from=build --chown=10001:10001 /app/dist ./dist
USER 10001:10001
EXPOSE 4330
CMD ["node", "dist/server/node.js"]
