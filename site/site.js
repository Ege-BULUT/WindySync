// Every page has an English URL and a Turkish one under /tr/; the TR/EN button links to the other.
// A saved choice (or a Turkish browser on the first visit) sends you to your language.
(() => {
  const KEY = "wysy-lang", isTr = document.documentElement.lang === "tr", btn = document.getElementById("lang");
  let saved = null;
  try { saved = localStorage.getItem(KEY); } catch (e) {}
  const want = saved ? saved === "tr" : isTr || (navigator.language || "").toLowerCase().startsWith("tr");
  if (want !== isTr && location.protocol !== "file:") location.replace(btn.getAttribute("href") + location.hash);
  btn.addEventListener("click", () => { try { localStorage.setItem(KEY, isTr ? "en" : "tr"); } catch (e) {} btn.href += location.hash; });
})();

const io = new IntersectionObserver(es => es.forEach(e => { if (e.isIntersecting) { e.target.classList.add("in"); io.unobserve(e.target); } }), { threshold: .12 });
document.querySelectorAll(".reveal").forEach(el => io.observe(el));
