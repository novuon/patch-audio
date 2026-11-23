// Every remote page uses this shell; add new destinations here once.
const pages = [
  {id:'dj', label:'AI', title:'AI DJ', href:'/remote/dj'},
  {id:'booth', label:'Booth', title:'DJ Booth', href:'/remote/booth'},
  {id:'mixer', label:'Mix', title:'Mixer Unit', href:'/remote/mixer'},
  {id:'spin', label:'Spin', title:'AI DJ Spin', href:'/remote/spin'},
  {id:'library', label:'Library', title:'Library', href:'/remote/library'},
  {id:'visual', label:'Visuals', title:'Visualiser', href:'/remote/visualiser'},
];
customElements.define('patch-navigation', class extends HTMLElement {
  connectedCallback() {
    if (this.firstChild) return;
    const current = pages.find(page => page.id === this.getAttribute('page'));
    const header = document.createElement('header');
    header.className = 'remote-header';
    const brand = document.createElement('a');
    brand.className = 'remote-brand';
    brand.href = '/remote/dj';
    brand.append('PATCH');
    const rule = document.createElement('span');
    rule.className = 'remote-brand-rule'; rule.textContent = '/';
    const title = document.createElement('span');
    title.textContent = current?.title ?? this.getAttribute('title') ?? 'REMOTE';
    brand.append(rule, title);
    const nav = document.createElement('nav');
    nav.setAttribute('aria-label', 'Remote pages');
    for (const page of pages) {
      const link = document.createElement('a');
      link.href = page.href; link.textContent = page.label;
      if (page === current) link.setAttribute('aria-current', 'page');
      nav.append(link);
    }
    header.append(brand, nav); this.append(header);
  }
});
