/* Detection is a hint only: every platform and download stays visible. */
(() => {
  const platform = (navigator.userAgentData?.platform || navigator.platform || '').toLowerCase();
  if (/android|iphone|ipad|cros/.test(navigator.userAgent.toLowerCase()) || (platform === 'macintel' && navigator.maxTouchPoints > 1)) return;
  const os = /win/.test(platform) ? 'windows' : /mac/.test(platform) ? 'mac' : /linux/.test(platform) ? 'linux' : null;
  if (!os) return;
  document.querySelector(`.os[data-os="${os}"]`)?.classList.add('detected');
})();
