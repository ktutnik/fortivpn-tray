const params = new URLSearchParams(window.location.search);
const profileId = params.get('profileId');
const profileName = params.get('profileName');

if (profileName) {
    document.getElementById('prompt-label').textContent = `Password for ${profileName}`;
}

document.getElementById('password').addEventListener('keydown', (e) => {
    if (e.key === 'Enter') submit();
    if (e.key === 'Escape') cancel();
});

async function submit() {
    const password = document.getElementById('password').value;
    const remember = document.getElementById('remember').checked;
    if (!password) return;

    try {
        await window.__TAURI__.core.invoke('cmd_submit_password', {
            profileId,
            password,
            remember,
        });
    } catch (e) {
        console.error('Submit failed:', e);
    }

    window.__TAURI__.event.emit('password-submitted', { profileId });
    window.__TAURI__.window.getCurrent().close();
}

function cancel() {
    window.__TAURI__.window.getCurrent().close();
}
