var d={healthy:"svc-healthy",degraded:"svc-degraded",inactive:"svc-inactive",failed:"svc-failed"},r={healthy:"\u25CF running",degraded:"\u25CF exited",inactive:"\u25CB inactive",failed:"\u2715 failed"};function l(e){let s=d[e.health]??"svc-failed",t=r[e.health]??e.health,c=e.description?`<div class="svc-description">${n(e.description)}</div>`:"",a=e.pid!=null?`<span class="svc-key">pid</span><span class="svc-val">${e.pid}</span>`:"",i=e.since!=null?`<span class="svc-key">since</span><span class="svc-val svc-timestamp">${n(e.since)}</span>`:"";return`
<div class="svc-card ${s}">
  <div class="svc-card-header">
    <span class="svc-name">${n(e.name)}</span>
    <span class="svc-badge ${s}">${t}</span>
  </div>
  ${c}
  <div class="svc-fields">
    <span class="svc-key">state</span>
    <span class="svc-val">${n(e.active_state)}/${n(e.sub_state)}</span>
    ${a}
    ${i}
  </div>
</div>`.trim()}function n(e){return e.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;").replace(/'/g,"&#x27;")}function o(e){return e.toLocaleTimeString()}async function p(e=fetch){let s=await e("/api/services");if(!s.ok)throw new Error(`/api/services returned ${s.status}`);return s.json()}function v(e,s,t){e.innerHTML=t.map(l).join(`
`),s.textContent=o(new Date)}if(typeof document<"u"){let e=document.getElementById("svc-grid"),s=document.getElementById("svc-last-updated"),t=document.getElementById("svc-refresh");async function c(){try{let a=await p();e&&s&&v(e,s,a)}catch(a){console.error("services refresh failed:",a)}}t&&t.addEventListener("click",()=>{c()}),setInterval(()=>{c()},15e3)}export{v as applyUpdate,n as escHtml,p as fetchStatuses,o as formatLastUpdated,l as renderCard};
