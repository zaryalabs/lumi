import { createServer } from "node:http";

const port = Number(process.env.LUMI_E2E_OPENROUTER_PORT ?? "19090");

const server = createServer((request, response) => {
  if (request.method === "GET" && request.url === "/health") {
    response.writeHead(200, { "content-type": "text/plain" });
    response.end("ok");
    return;
  }
  if (request.method === "POST" && request.url === "/v1/audio/transcriptions") {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    request.on("end", () => {
      if (request.headers.authorization !== "Bearer sk-e2e-openai") {
        response.writeHead(401, { "content-type": "application/json" });
        response.end(JSON.stringify({ error: { code: "invalid_api_key" } }));
        return;
      }
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          text: "Reader core не зависит от DOM, Dioxus и platform handles.",
          language: "ru",
          duration: 1.25,
        }),
      );
    });
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
      if (body.response_format?.type === "json_object") {
        const sourceMessage =
          body.messages.find((message) =>
            message.content?.includes("<source citation_id="),
          )?.content ?? "";
        const citationId =
          sourceMessage.match(/<source citation_id="([^"]+)">/)?.[1] ??
          "ctx:missing:1";
        const isAbridgement = body.messages.some((message) =>
          message.content?.includes("abridgement-artifact.v1"),
        );
        response.writeHead(200, { "content-type": "application/json" });
        response.end(
          JSON.stringify({
            id: isAbridgement ? "structured-abridgement" : "structured-summary",
            choices: [
              {
                message: {
                  content: JSON.stringify(
                    isAbridgement
                      ? {
                          schema_version: "abridgement-artifact.v1",
                          title: "Сокращённое руководство Lumi",
                          profile: "balanced",
                          chapters: [
                            {
                              title: "Главное",
                              content:
                                "Ключевые идеи материала в проверяемом сокращении.",
                              citation_ids: [citationId],
                            },
                          ],
                          citation_ids: [citationId],
                        }
                      : {
                          schema_version: "summary-artifact.v1",
                          content:
                            "Краткое саммари фикстуры с проверяемым источником.",
                          citation_ids: [citationId],
                        },
                  ),
                },
              },
            ],
          }),
        );
        return;
      }
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
