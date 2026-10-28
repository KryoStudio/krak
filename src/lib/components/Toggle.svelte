<script lang="ts">
  let { checked, label, onchange }: { checked: boolean; label: string; onchange: (value: boolean) => void } = $props();
</script>

<button role="switch" aria-checked={checked} aria-label={label} class:on={checked} onclick={() => onchange(!checked)}>
  <span></span>
</button>

<style>
  button {
    position: relative;
    width: 38px;
    height: 22px;
    flex: none;
    border-radius: 999px;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px var(--border-strong);
    transition:
      background var(--normal),
      box-shadow var(--normal);
  }
  span {
    position: absolute;
    top: 3px;
    left: 3px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--text-2);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.4);
    transition:
      transform var(--slow) var(--ease-out),
      background var(--normal),
      width var(--fast) var(--ease-out);
  }
  button:active span {
    width: 20px;
  }
  .on {
    background: var(--accent);
    box-shadow: none;
  }
  .on span {
    background: #fff;
    transform: translateX(16px);
  }
  .on:active span {
    transform: translateX(12px);
  }
</style>
