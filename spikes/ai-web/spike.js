import "./node_modules/deep-chat/dist/deepChat.bundle.js";

const chat = document.querySelector("#chat");
const restored = JSON.parse(
  sessionStorage.getItem("lumi-stage0-chat-history") ??
    '[{"role":"ai","text":"История загружена с сервера."}]',
);

chat.history = restored;
chat.remarkable = { html: false, linkTarget: "_blank" };
chat.textInput = {
  placeholder: { text: "Спросить о выбранном фрагменте" },
  characterLimit: 2000,
};
chat.messageStyles = {
  default: {
    user: { bubble: { backgroundColor: "#dce8da", color: "#1f2b22" } },
    ai: { bubble: { backgroundColor: "#f0eadc", color: "#1f2b22" } },
  },
};
chat.connect = {
  stream: true,
  handler: (_body, signals) => {
    const chunks = ["Потоковый ", "ответ ", "с цитатой [1]."];
    let stopped = false;
    let index = 0;
    signals.onOpen();

    const timer = window.setInterval(() => {
      if (stopped || index >= chunks.length) {
        window.clearInterval(timer);
        signals.onClose();
        if (!stopped) {
          document.body.dataset.stream = "complete";
        }
        return;
      }
      signals.onResponse({ text: chunks[index] });
      index += 1;
    }, 80);

    signals.stopClicked.listener = () => {
      stopped = true;
      window.clearInterval(timer);
      signals.onClose();
      document.body.dataset.stream = "stopped";
    };
  },
};
chat.onMessage = () => {
  const messages = chat.getMessages();
  sessionStorage.setItem("lumi-stage0-chat-history", JSON.stringify(messages));
};
chat.onComponentRender = () => {
  document.body.dataset.rendered = "true";
};

window.lumiDeepChatSpike = {
  chat,
  attachSelection() {
    chat.addMessage(
      {
        role: "user",
        text: "Контекст: «явно выбранный фрагмент»",
        custom: {
          source_ref: "fixture:revision-1:block-1",
        },
      },
      true,
    );
  },
};
