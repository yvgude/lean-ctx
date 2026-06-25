import { IncomingMessage, ServerResponse } from "http";

interface User {
  id: number;
  name: string;
  email: string;
}

interface ApiResponse<T> {
  data: T | null;
  error: string | null;
  status: number;
}

function jsonResponse<T>(res: ServerResponse, body: ApiResponse<T>): void {
  res.writeHead(body.status, { "Content-Type": "application/json" });
  res.end(JSON.stringify(body));
}

class UserHandler {
  private users: Map<number, User> = new Map();
  private nextId = 1;

  constructor() {
    this.seed();
  }

  private seed(): void {
    this.users.set(1, { id: 1, name: "Alice", email: "alice@test.com" });
    this.users.set(2, { id: 2, name: "Bob", email: "bob@test.com" });
    this.nextId = 3;
  }

  @logMethod
  async list(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const data = Array.from(this.users.values());
    jsonResponse(res, { data, error: null, status: 200 });
  }

  @logMethod
  async getById(req: IncomingMessage, res: ServerResponse, id: number): Promise<void> {
    const user = this.users.get(id);
    if (!user) {
      jsonResponse(res, { data: null, error: "User not found", status: 404 });
      return;
    }
    jsonResponse(res, { data: user, error: null, status: 200 });
  }

  async create(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const user: User = {
      id: this.nextId++,
      name: `User_${this.nextId}`,
      email: `user${this.nextId}@test.com`,
    };
    this.users.set(user.id, user);
    jsonResponse(res, { data: user, error: null, status: 201 });
  }
}

function logMethod(target: any, propertyKey: string, descriptor: PropertyDescriptor) {
  const original = descriptor.value;
  descriptor.value = function (...args: any[]) {
    console.log(`Calling ${propertyKey} with`, args);
    return original.apply(this, args);
  };
  return descriptor;
}

export { UserHandler, User, ApiResponse, jsonResponse };
