var o={healthy:"svc-healthy",degraded:"svc-degraded",inactive:"svc-inactive",failed:"svc-failed"},p={healthy:"\u25CF running",degraded:"\u25CF exited",inactive:"\u25CB inactive",failed:"\u2715 failed"};function v(e){let s=o[e.health]??"svc-failed",a=p[e.health]??e.health,c=e.description?`<div class="svc-description">${t(e.description)}</div>`:"",n=e.pid!=null?`<span class="svc-key">pid</span><span class="svc-val">${e.pid}</span>`:"",i=e.since!=null?`<span class="svc-key">since</span><span class="svc-val svc-timestamp">${t(e.since)}</span>`:"",r=e.icon_url??"/assets/img/service.svg",d=`<img src="${t(r)}" alt="" class="svc-icon" aria-hidden="true" width="18" height="18">`,l=e.url?`<a href="${t(e.url)}" class="svc-name svc-link">${t(e.name)}</a>`:`<span class="svc-name">${t(e.name)}</span>`;return`
<div class="svc-card ${s}">
  <div class="svc-card-header">
    ${d}
    ${l}
    <span class="svc-badge ${s}">${a}</span>
  </div>
  ${c}
  <div class="svc-fields">
    <span class="svc-key">state</span>
    <span class="svc-val">${t(e.active_state)}/${t(e.sub_state)}</span>
    ${n}
    ${i}
  </div>
</div>`.trim()}function t(e){return e.replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;").replace(/'/g,"&#x27;")}function u(e){return e.toLocaleTimeString()}async function h(e=fetch){let s=await e("/api/services");if(!s.ok)throw new Error(`/api/services returned ${s.status}`);return s.json()}function f(e,s,a){e.innerHTML=a.map(v).join(`
`),s.textContent=u(new Date)}if(typeof document<"u"){let e=document.getElementById("svc-grid"),s=document.getElementById("svc-last-updated"),a=document.getElementById("svc-refresh");async function c(){try{let n=await h();e&&s&&f(e,s,n)}catch(n){console.error("services refresh failed:",n)}}a&&a.addEventListener("click",()=>{c()}),setInterval(()=>{c()},15e3)}export{f as applyUpdate,t as escHtml,h as fetchStatuses,u as formatLastUpdated,v as renderCard};
