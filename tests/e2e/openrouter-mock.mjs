import { createServer } from "node:http";

const port = Number(process.env.LUMI_E2E_OPENROUTER_PORT ?? "19090");

const server = createServer((request, response) => {
  if (request.method === "GET" && request.url === "/health") {
    response.writeHead(200, { "content-type": "text/plain" });
    response.end("ok");
    return;
  }
  if (request.method !== "POST" || request.url !== "/api/v1/chat/completions") {
    response.writeHead(404);
    response.end();
    return;
  }

  const chunks = [];
  request.on("data", (chunk) => chunks.push(chunk));
  request.on("end", () => {
    if (request.headers.authorization !== "Bearer sk-e2e-openrouter") {
      response.writeHead(401, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { code: "invalid_api_key" } }));
      return;
    }
    const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    if (!body.stream) {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          id: "validation",
          choices: [{ message: { content: "OK" } }],
        }),
      );
      return;
    }

    response.writeHead(200, {
      "cache-control": "no-cache",
      connection: "keep-alive",
      "content-type": "text/event-stream",
    });
    const frames = [
      {
        id: "generation",
        choices: [
          {
            delta: {
              content: "Ответ фикстуры основан на прикреплённом источнике.",
            },
            finish_reason: null,
          },
        ],
      },
      {
        id: "generation",
        choices: [{ delta: {}, finish_reason: "stop" }],
        usage: { prompt_tokens: 24, completion_tokens: 8 },
      },
    ];
    for (const frame of frames) {
      response.write(`data: ${JSON.stringify(frame)}\n\n`);
    }
    response.end("data: [DONE]\n\n");
  });
});

server.listen(port, "127.0.0.1");

for (const signal of ["SIGINT", "SIGTERM"]) {
  process.on(signal, () => server.close(() => process.exit(0)));
}
