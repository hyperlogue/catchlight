/** A private starter is served only by the loopback development server. */
import { createReadStream } from "node:fs";
import { open, realpath } from "node:fs/promises";
import { basename, extname, resolve } from "node:path";
import type { IncomingMessage } from "node:http";
import type { Plugin } from "vite";

const routeFile = "__catchlight_local_example.clm";
const loopbackHosts = new Set(["127.0.0.1", "localhost", "::1", "[::1]"]);

function sameOrigin(request: IncomingMessage, protocol: string): boolean {
  const host = request.headers.host;
  if (!host) return false;
  const site = request.headers["sec-fetch-site"];
  if (site && site !== "same-origin" && site !== "none") return false;
  try {
    const origin = new URL(`${protocol}://${host}`);
    return loopbackHosts.has(origin.hostname) &&
      (!request.headers.origin || request.headers.origin === origin.origin);
  } catch {
    return false;
  }
}

export async function localExample(configuredPath: string): Promise<{
  example: { file: string; name: string };
  plugin: Plugin;
}> {
  if (extname(configuredPath).toLowerCase() !== ".clm")
    throw new Error("CATCHLIGHT_LOCAL_MODEL must name a .clm file.");
  let path: string;
  try {
    path = await realpath(resolve(configuredPath));
    const file = await open(path, "r");
    try {
      if (!(await file.stat()).isFile()) throw new Error("Not a regular file.");
    } finally {
      await file.close();
    }
  } catch (cause) {
    throw new Error("CATCHLIGHT_LOCAL_MODEL must name an existing, readable file.", { cause });
  }
  return {
    example: { file: routeFile, name: basename(configuredPath) },
    plugin: {
      name: "catchlight-local-example",
      apply: "serve",
      configureServer(server) {
        const host = server.config.server.host;
        if (typeof host !== "string" || !loopbackHosts.has(host))
          throw new Error("CATCHLIGHT_LOCAL_MODEL requires a loopback server host.");
        const base = new URL(server.config.base, "http://localhost").pathname;
        const route = `${base}${routeFile}`;
        const protocol = server.config.server.https ? "https" : "http";
        server.middlewares.use((request, response, next) => {
          if (request.url?.split("?")[0] !== route) return next();
          response.setHeader("Cache-Control", "no-store");
          response.setHeader("Cross-Origin-Resource-Policy", "same-origin");
          response.setHeader("X-Content-Type-Options", "nosniff");
          if (!sameOrigin(request, protocol)) {
            response.statusCode = 403;
            response.end("The local example is available only from this development origin.");
            return;
          }
          if (request.method !== "GET" && request.method !== "HEAD") {
            response.statusCode = 405;
            response.setHeader("Allow", "GET, HEAD");
            response.end();
            return;
          }
          response.setHeader("Content-Type", "application/octet-stream");
          if (request.method === "HEAD") {
            response.end();
            return;
          }
          const stream = createReadStream(path);
          stream.on("error", () => {
            if (response.destroyed) return;
            if (response.headersSent) response.destroy();
            else {
              response.statusCode = 500;
              response.end("The local example could not be read.");
            }
          });
          response.on("close", () => stream.destroy());
          stream.pipe(response);
        });
      },
    },
  };
}
