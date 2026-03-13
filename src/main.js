const { invoke } = window.__TAURI__.core;

let profiles = [];
let selectedId = null;
let isNew = false;

const $ = (sel) => document.querySelector(sel);

async function loadProfiles() {
  profiles = await invoke("get_profiles");
  renderTabs();
  if (profiles.length > 0 && !selectedId) {
    selectProfile(profiles[0].id);
  } else if (selectedId) {
    selectProfile(selectedId);
  } else {
    showNewForm();
  }
}

function renderTabs() {
  const tabs = $("#tabs");
  // Clear tabs safely
  while (tabs.firstChild) tabs.removeChild(tabs.firstChild);

  for (const p of profiles) {
    const tab = document.createElement("button");
    tab.type = "button";
    tab.className = "tab" + (p.id === selectedId && !isNew ? " active" : "");
    tab.textContent = p.name;
    tab.addEventListener("click", () => selectProfile(p.id));
    tabs.appendChild(tab);
  }

  const addTab = document.createElement("button");
  addTab.type = "button";
  addTab.className = "tab tab-add" + (isNew ? " active" : "");
  addTab.textContent = "+";
  addTab.addEventListener("click", showNewForm);
  tabs.appendChild(addTab);
}

function selectProfile(id) {
  const p = profiles.find((x) => x.id === id);
  if (!p) return;

  selectedId = id;
  isNew = false;
  renderTabs();

  $("#f-name").value = p.name;
  $("#f-host").value = p.host;
  $("#f-port").value = p.port;
  $("#f-username").value = p.username;
  $("#f-cert").value = p.trusted_cert;
  $("#btn-delete").style.display = "";
  $("#pw-input").style.display = "none";
  $("#password-section").style.display = "";
  clearMessage();

  checkPassword(id);
}

function showNewForm() {
  selectedId = null;
  isNew = true;
  renderTabs();

  $("#f-name").value = "";
  $("#f-host").value = "";
  $("#f-port").value = "10443";
  $("#f-username").value = "";
  $("#f-cert").value = "";
  $("#btn-delete").style.display = "none";
  $("#password-section").style.display = "none";
  $("#pw-input").style.display = "none";
  clearMessage();
}

async function checkPassword(id) {
  const has = await invoke("has_password", { id });
  const indicator = $("#pw-indicator");
  indicator.className = has ? "dot dot-green" : "dot dot-red";
  indicator.title = has ? "Password is set" : "No password set";
}

function showMessage(text, type) {
  const el = $("#message");
  el.textContent = text;
  el.className = "message " + type;
  setTimeout(() => clearMessage(), 3000);
}

function clearMessage() {
  const el = $("#message");
  el.textContent = "";
  el.className = "message";
}

// Save profile
$("#profile-form").addEventListener("submit", async (e) => {
  e.preventDefault();

  const profile = {
    id: isNew ? null : selectedId,
    name: $("#f-name").value.trim(),
    host: $("#f-host").value.trim(),
    port: parseInt($("#f-port").value, 10),
    username: $("#f-username").value.trim(),
    trusted_cert: $("#f-cert").value.trim(),
  };

  if (!profile.name || !profile.host || !profile.username) {
    showMessage("Name, Host, and Username are required", "error");
    return;
  }

  try {
    const id = await invoke("save_profile", { profile });
    selectedId = id;
    isNew = false;
    await loadProfiles();
    showMessage("Profile saved", "success");
  } catch (err) {
    showMessage("Failed: " + err, "error");
  }
});

// Delete profile
$("#btn-delete").addEventListener("click", async () => {
  if (!selectedId) return;
  const p = profiles.find((x) => x.id === selectedId);
  if (!confirm(`Delete profile "${p?.name}"?`)) return;

  try {
    await invoke("delete_profile", { id: selectedId });
    selectedId = null;
    await loadProfiles();
    showMessage("Profile deleted", "success");
  } catch (err) {
    showMessage("Failed: " + err, "error");
  }
});

// Change password toggle
$("#btn-change-pw").addEventListener("click", () => {
  const el = $("#pw-input");
  el.style.display = el.style.display === "none" ? "flex" : "none";
  if (el.style.display === "flex") {
    $("#f-password").value = "";
    $("#f-password").focus();
  }
});

// Set password
$("#btn-set-pw").addEventListener("click", async () => {
  const password = $("#f-password").value;
  if (!password) {
    showMessage("Password cannot be empty", "error");
    return;
  }

  try {
    await invoke("cmd_set_password", { id: selectedId, password });
    $("#pw-input").style.display = "none";
    $("#f-password").value = "";
    checkPassword(selectedId);
    showMessage("Password updated", "success");
  } catch (err) {
    showMessage("Failed: " + err, "error");
  }
});

// Init
window.addEventListener("DOMContentLoaded", loadProfiles);
