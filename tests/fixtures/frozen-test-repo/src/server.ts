import { IncomingMessage, ServerResponse, createServer } from "http";
import { URL } from "url";

interface RouteHandler {
  (req: IncomingMessage, res: ServerResponse): void | Promise<void>;
}

interface Route {
  method: "GET" | "POST" | "PUT" | "DELETE";
  path: string;
  handler: RouteHandler;
}

class Router {
  private routes: Route[] = [];

  get(path: string, handler: RouteHandler): void {
    this.routes.push({ method: "GET", path, handler });
  }

  post(path: string, handler: RouteHandler): void {
    this.routes.push({ method: "POST", path, handler });
  }

  match(method: string, url: string): RouteHandler | null {
    const parsed = new URL(url, "http://localhost");
    for (const route of this.routes) {
      if (route.method === method && route.path === parsed.pathname) {
        return route.handler;
      }
    }
    return null;
  }
}

class HttpServer {
  private router = new Router();

  constructor() {
    this.router.get("/health", async (_req, res) => {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.end(JSON.stringify({ status: "ok" }));
    });
  }

  async start(port: number): Promise<void> {
    const server = createServer((req, res) => {
      const handler = this.router.match(req.method!, req.url!);
      if (handler) {
        handler(req, res);
      } else {
        res.writeHead(404);
        res.end("Not Found");
      }
    });
    return new Promise((resolve) => {
      server.listen(port, () => {
        console.log(`Server listening on :${port}`);
        resolve();
      });
    });
  }
}

export { HttpServer, Router, Route, RouteHandler };
